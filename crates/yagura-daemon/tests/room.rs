#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_daemon::room`: the pure functions and types behind the shared chat
//! room -- mention
//! resolution, the repetition / monologue / turn-budget halt rules, the
//! silence candidate rule, the chat envelope, and the `[chat]` limits'
//! defaults and range check.
//!
//! Nothing here touches a PTY, a store or the wire protocol: every function
//! under test takes plain values and returns plain values, so all of it can
//! be checked without a window, a connection or a clock.
//!
//! This file also fixes the parts of the contract that carry no signature.
//! Each such choice is called out where it is used.

use yagura_daemon::room::{
    ChatDeliveryState, ChatEnvelopeFrom, ChatEnvelopeTooLong, ChatLimits, ChatLimitsError,
    DeliveryOutcome, HaltReason, HistoryEntry, RosterEntry, SilenceCheck, Speaker,
    compose_chat_envelope, evaluate_limits, is_silence_candidate, parse_mentions,
};
use yagura_protocol::{HANDOFF_ENVELOPE_MAX_BYTES, UnknownReason};

// =====================================================================
// Shared fixture helpers
// =====================================================================

fn roster(entries: &[(&str, &str)]) -> Vec<RosterEntry> {
    entries
        .iter()
        .map(|(window_id, name)| RosterEntry {
            window_id: (*window_id).to_string(),
            name: (*name).to_string(),
        })
        .collect()
}

// =====================================================================
// Mention resolution
//
// `parse_mentions(text, roster, self_window_id) -> Mentions { targets, user,
// unresolved }`. The architecture's own signature only carries `text` and
// `roster`, but its own prose requires a self-window a message's own mention
// be dropped from `targets` without turning into `unresolved`, which is not
// otherwise derivable from `text` and `roster` alone. This file settles that
// gap by adding a third `self_window_id: &str` parameter (appended last, so
// the other two keep the documented order).
//
// `targets` carries resolved window ids (the daemon's addressing key),
// deduplicated and in first-occurrence order. `unresolved` carries the raw
// mention text (quotes stripped, `@` stripped), also deduplicated and in
// first-occurrence order.
// =====================================================================

#[test]
fn resolves_by_exact_name_match() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("ping @Bob please", &people, "w-a");

    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
    assert!(!mentions.user);
    assert!(mentions.unresolved.is_empty());
}

#[test]
fn resolves_by_window_id_when_it_does_not_match_any_name() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("ping @w-b please", &people, "w-a");

    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
    assert!(mentions.unresolved.is_empty());
}

#[test]
fn matching_is_case_insensitive_and_treats_underscore_and_hyphen_as_equivalent() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "codex_1")]);
    let mentions = parse_mentions("ping @CODEX-1 please", &people, "w-a");

    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
}

#[test]
fn a_quoted_mention_with_spaces_is_parsed_as_a_single_token() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "the reviewer")]);
    let mentions = parse_mentions("ping @\"the reviewer\" please", &people, "w-a");

    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
}

#[test]
fn an_unquoted_mention_is_cut_at_the_first_boundary_punctuation() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("ping @Bob, are you there", &people, "w-a");

    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
}

#[test]
fn at_user_sets_the_user_flag_and_is_neither_a_target_nor_unresolved() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("ping @user please review", &people, "w-a");

    assert!(mentions.user);
    assert!(mentions.targets.is_empty());
    assert!(mentions.unresolved.is_empty());
}

#[test]
fn at_user_reservation_is_case_insensitive() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("ping @USER please review", &people, "w-a");

    assert!(mentions.user);
}

#[test]
fn at_all_in_the_body_is_ignored_and_leaves_no_trace() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let with_all = parse_mentions("hi @all team, @Bob is on it", &people, "w-a");
    let without_all = parse_mentions("hi team, @Bob is on it", &people, "w-a");

    assert_eq!(with_all.targets, without_all.targets);
    assert_eq!(with_all.user, without_all.user);
    assert_eq!(with_all.unresolved, without_all.unresolved);
    assert!(!with_all.user);
}

#[test]
fn a_name_matching_more_than_one_roster_entry_is_unresolved() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob"), ("w-c", "Bob")]);
    let mentions = parse_mentions("ping @Bob please", &people, "w-a");

    assert!(mentions.targets.is_empty());
    assert_eq!(mentions.unresolved, vec!["Bob".to_string()]);
}

#[test]
fn a_name_absent_from_the_roster_is_unresolved() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("ping @Carol please", &people, "w-a");

    assert!(mentions.targets.is_empty());
    assert_eq!(mentions.unresolved, vec!["Carol".to_string()]);
}

#[test]
fn a_window_named_after_a_reserved_word_is_reachable_only_by_id() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "user")]);
    let by_name = parse_mentions("ping @user please", &people, "w-a");
    let by_id = parse_mentions("ping @w-b please", &people, "w-a");

    assert!(
        by_name.targets.is_empty(),
        "a window literally named \"user\" cannot be reached by that name: it reads as \
         the reserved word instead"
    );
    assert!(by_name.user);
    assert_eq!(by_id.targets, vec!["w-b".to_string()]);
    assert!(!by_id.user);
}

#[test]
fn a_self_mention_is_dropped_from_targets_without_becoming_unresolved() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("ping @Alice and @Bob", &people, "w-a");

    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
    assert!(mentions.unresolved.is_empty());
}

#[test]
fn duplicate_mentions_are_deduplicated_preserving_first_occurrence_order() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob"), ("w-c", "Carol")]);
    let mentions = parse_mentions("@Carol @Bob @Carol @Bob", &people, "w-a");

    assert_eq!(
        mentions.targets,
        vec!["w-c".to_string(), "w-b".to_string()],
        "each target appears once, ordered by its first mention in the text"
    );
}

// =====================================================================
// `@parent` mention
//
// `parent` joins `user` as a reserved word: recognized case-insensitively,
// never resolved as a name or an id, and set on its own `Mentions.parent`
// flag (parallel to `Mentions.user`). Resolving *who* the parent is (the
// window's opener, or a task-bound window's plan owner) is not this pure
// function's job -- `post_chat` does that once a message actually reaches
// the daemon (see `tests/chat_parent.rs`); this file only pins the parsing
// contract `parse_mentions` must hold before that resolution can run.
// =====================================================================

#[test]
fn at_parent_sets_the_parent_flag_and_is_neither_a_target_nor_unresolved() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("ping @parent please review", &people, "w-a");

    assert!(mentions.parent);
    assert!(mentions.targets.is_empty());
    assert!(mentions.unresolved.is_empty());
}

#[test]
fn at_parent_reservation_is_case_insensitive() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("ping @PARENT please review", &people, "w-a");

    assert!(mentions.parent);
}

#[test]
fn a_window_named_after_the_parent_reserved_word_is_reachable_only_by_id() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "parent")]);
    let by_name = parse_mentions("ping @parent please", &people, "w-a");
    let by_id = parse_mentions("ping @w-b please", &people, "w-a");

    assert!(
        by_name.targets.is_empty(),
        "a window literally named \"parent\" cannot be reached by that name: it reads as \
         the reserved word instead"
    );
    assert!(by_name.parent);
    assert_eq!(by_id.targets, vec!["w-b".to_string()]);
    assert!(!by_id.parent);
}

#[test]
fn at_parent_and_at_user_are_independent_flags() {
    let people = roster(&[("w-a", "Alice")]);
    let mentions = parse_mentions("ping @parent and @user please", &people, "w-a");

    assert!(
        mentions.parent,
        "the @parent flag must not depend on @user also appearing"
    );
    assert!(
        mentions.user,
        "the @user flag must not depend on @parent also appearing"
    );
}

#[test]
fn an_at_sign_inside_an_email_address_is_not_a_mention() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "host")]);
    let mentions = parse_mentions("write to bob@host today", &people, "w-a");

    assert!(mentions.targets.is_empty(), "{:?}", mentions.targets);
    assert!(mentions.unresolved.is_empty(), "{:?}", mentions.unresolved);
}

#[test]
fn two_quoted_mentions_in_one_message_each_resolve_to_their_own_window() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob"), ("w-c", "Bo")]);
    let mentions = parse_mentions("ping @\"Bo\" and @\"Bob\"", &people, "w-a");

    assert_eq!(mentions.targets, vec!["w-c".to_string(), "w-b".to_string()]);
    assert!(mentions.unresolved.is_empty(), "{:?}", mentions.unresolved);
}

#[test]
fn an_unterminated_quoted_mention_addresses_nobody() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("ping @\"Bob", &people, "w-a");

    assert!(mentions.targets.is_empty(), "{:?}", mentions.targets);
    assert!(mentions.unresolved.is_empty(), "{:?}", mentions.unresolved);
}

#[test]
fn a_quoted_name_ending_in_an_at_sign_does_not_swallow_the_mention_after_it() {
    let people = roster(&[("w-a", "Alice"), ("w-b", "Bob")]);
    let mentions = parse_mentions("@\"x @\" ping @Bob \"ok\"", &people, "w-a");

    assert_eq!(mentions.targets, vec!["w-b".to_string()]);
    assert_eq!(mentions.unresolved, vec!["x @".to_string()]);
}

// =====================================================================
// The halt rules
//
// `evaluate_limits(history, &ChatLimits) -> Option<HaltReason>`. This file
// settles the shape of `history` (a slice of `HistoryEntry`, oldest first,
// with the message under judgement as the last element -- matching the
// order `append_message` reads it back in) and of `HaltReason`
// (`Repetition` / `Monologue` / `TurnBudget`).
//
// `HistoryEntry { speaker, mentions_user, targets, body }`: `speaker`
// distinguishes a window (by id), the user, an external caller and a
// system entry; `mentions_user` and `targets` are the position key's other
// two components ("same speaker, same sorted target set, same @user
// flag"); `body` is what the repetition distance is computed
// over.
//
// Each helper below builds a `ChatLimits` that only enables the one rule
// its test exercises (the other two thresholds set far out of reach), so a
// history built to trip one rule cannot be read as tripping another.
// =====================================================================

fn limits_for_repetition() -> ChatLimits {
    ChatLimits {
        repetition_count: 4,
        repetition_window: 12,
        repetition_distance: 6,
        monologue_max: 1_000,
        turn_message_max: 1_000,
        silence_timeout_secs: 300,
        history_max: 5_000,
    }
}

fn limits_for_monologue() -> ChatLimits {
    ChatLimits {
        repetition_count: 1_000,
        repetition_window: 1_000,
        repetition_distance: 0,
        monologue_max: 3,
        turn_message_max: 1_000,
        silence_timeout_secs: 300,
        history_max: 5_000,
    }
}

fn limits_for_turn_budget() -> ChatLimits {
    ChatLimits {
        repetition_count: 1_000,
        repetition_window: 1_000,
        repetition_distance: 0,
        monologue_max: 1_000,
        turn_message_max: 30,
        silence_timeout_secs: 300,
        history_max: 5_000,
    }
}

fn repetition_entry(body: &str, targets: &[&str]) -> HistoryEntry {
    HistoryEntry {
        speaker: Speaker::Window("w-a".to_string()),
        mentions_user: false,
        targets: targets.iter().map(|t| (*t).to_string()).collect(),
        body: body.to_string(),
    }
}

fn plain_entry(speaker: Speaker, body: &str) -> HistoryEntry {
    HistoryEntry {
        speaker,
        mentions_user: false,
        targets: Vec::new(),
        body: body.to_string(),
    }
}

#[test]
fn repetition_halts_once_the_same_position_reaches_the_configured_count() {
    let history: Vec<HistoryEntry> = (0..4)
        .map(|_| repetition_entry("please rebuild the project", &["w-b"]))
        .collect();

    assert_eq!(
        evaluate_limits(&history, &limits_for_repetition()),
        Some(HaltReason::Repetition)
    );
}

#[test]
fn repetition_does_not_halt_one_short_of_the_configured_count() {
    let history: Vec<HistoryEntry> = (0..3)
        .map(|_| repetition_entry("please rebuild the project", &["w-b"]))
        .collect();

    assert_eq!(evaluate_limits(&history, &limits_for_repetition()), None);
}

#[test]
fn repetition_counts_a_lightly_reworded_repeat_within_the_configured_distance() {
    let mut history: Vec<HistoryEntry> = (0..3)
        .map(|_| repetition_entry("the deployment failed again on staging", &["w-b"]))
        .collect();
    history.push(repetition_entry(
        "again on staging the deployment failed",
        &["w-b"],
    ));

    assert_eq!(
        evaluate_limits(&history, &limits_for_repetition()),
        Some(HaltReason::Repetition),
        "a reordering of the same words must still count as the same position"
    );
}

#[test]
fn repetition_does_not_count_a_clearly_different_message() {
    let mut history: Vec<HistoryEntry> = (0..3)
        .map(|_| repetition_entry("the deployment failed again on staging", &["w-b"]))
        .collect();
    history.push(repetition_entry(
        "coffee machine needs a new filter today",
        &["w-b"],
    ));

    assert_eq!(
        evaluate_limits(&history, &limits_for_repetition()),
        None,
        "unrelated content must not be counted toward the same position"
    );
}

#[test]
fn repetition_counts_a_repeat_with_one_extra_word_at_the_default_distance() {
    let mut history: Vec<HistoryEntry> = (0..3)
        .map(|_| repetition_entry("the deployment failed again on staging", &["w-b"]))
        .collect();
    history.push(repetition_entry(
        "build the deployment failed again on staging",
        &["w-b"],
    ));

    assert_eq!(
        evaluate_limits(&history, &limits_for_repetition()),
        Some(HaltReason::Repetition),
        "one added word keeps the body within the default similarity distance"
    );
}

#[test]
fn repetition_treats_full_width_case_and_punctuation_variants_as_the_same_position() {
    let mut history = vec![
        repetition_entry("please rebuild the project", &["w-b"]),
        repetition_entry("please rebuild the project", &["w-b"]),
        repetition_entry("PLEASE REBUILD THE PROJECT!!", &["w-b"]),
    ];
    history.push(repetition_entry(
        "\u{ff30}\u{ff2c}\u{ff25}\u{ff21}\u{ff33}\u{ff25} rebuild the project.",
        &["w-b"],
    ));

    assert_eq!(
        evaluate_limits(&history, &limits_for_repetition()),
        Some(HaltReason::Repetition),
        "full-width letters, case and trailing punctuation must normalize to the same text"
    );
}

#[test]
fn repetition_ignores_a_recurrence_that_falls_outside_the_window() {
    let mut history = vec![
        repetition_entry("please rebuild the project", &["w-b"]),
        repetition_entry("please rebuild the project", &["w-b"]),
        repetition_entry("please rebuild the project", &["w-b"]),
    ];
    for i in 0..12 {
        history.push(plain_entry(
            Speaker::Window("w-a".to_string()),
            &format!("unrelated filler message {i}"),
        ));
    }
    history.push(repetition_entry("please rebuild the project", &["w-b"]));

    assert_eq!(
        evaluate_limits(&history, &limits_for_repetition()),
        None,
        "the first three matches fell out of the repetition window by the time the \
         fourth one arrived"
    );
}

#[test]
fn repetition_requires_the_same_target_set() {
    let history = vec![
        repetition_entry("please rebuild the project", &["w-b"]),
        repetition_entry("please rebuild the project", &["w-c"]),
        repetition_entry("please rebuild the project", &["w-d"]),
        repetition_entry("please rebuild the project", &["w-e"]),
    ];

    assert_eq!(
        evaluate_limits(&history, &limits_for_repetition()),
        None,
        "each message was addressed to a different target set, so none of them share a \
         position with any other"
    );
}

#[test]
fn repetition_position_key_ignores_the_order_of_the_target_set() {
    let history = vec![
        repetition_entry("please rebuild the project", &["w-b", "w-c"]),
        repetition_entry("please rebuild the project", &["w-c", "w-b"]),
        repetition_entry("please rebuild the project", &["w-b", "w-c"]),
        repetition_entry("please rebuild the project", &["w-c", "w-b"]),
    ];

    assert_eq!(
        evaluate_limits(&history, &limits_for_repetition()),
        Some(HaltReason::Repetition),
        "the target set is compared sorted, so listing the same two targets in a \
         different order must not create a new position"
    );
}

#[test]
fn repetition_window_does_not_count_system_entries_toward_its_size() {
    let mut history = vec![
        repetition_entry("please rebuild the project", &["w-b"]),
        repetition_entry("please rebuild the project", &["w-b"]),
        repetition_entry("please rebuild the project", &["w-b"]),
    ];
    for _ in 0..20 {
        history.push(plain_entry(Speaker::System, "the room is quiet"));
    }
    history.push(repetition_entry("please rebuild the project", &["w-b"]));

    assert_eq!(
        evaluate_limits(&history, &limits_for_repetition()),
        Some(HaltReason::Repetition),
        "system entries do not occupy a slot in the repetition window, no matter how many \
         of them sit between two matching messages"
    );
}

#[test]
fn monologue_halts_once_the_same_speaker_reaches_the_configured_streak() {
    let history = vec![
        plain_entry(Speaker::Window("w-a".to_string()), "one"),
        plain_entry(Speaker::Window("w-a".to_string()), "two"),
        plain_entry(Speaker::Window("w-a".to_string()), "three"),
    ];

    assert_eq!(
        evaluate_limits(&history, &limits_for_monologue()),
        Some(HaltReason::Monologue)
    );
}

#[test]
fn monologue_streak_resets_when_a_different_speaker_intervenes() {
    let history = vec![
        plain_entry(Speaker::Window("w-a".to_string()), "one"),
        plain_entry(Speaker::Window("w-a".to_string()), "two"),
        plain_entry(Speaker::Window("w-b".to_string()), "hello"),
        plain_entry(Speaker::Window("w-a".to_string()), "three"),
        plain_entry(Speaker::Window("w-a".to_string()), "four"),
    ];

    assert_eq!(evaluate_limits(&history, &limits_for_monologue()), None);
}

#[test]
fn monologue_streak_is_not_broken_by_a_system_entry() {
    let history = vec![
        plain_entry(Speaker::Window("w-a".to_string()), "one"),
        plain_entry(Speaker::System, "the room is quiet"),
        plain_entry(Speaker::Window("w-a".to_string()), "two"),
        plain_entry(Speaker::Window("w-a".to_string()), "three"),
    ];

    assert_eq!(
        evaluate_limits(&history, &limits_for_monologue()),
        Some(HaltReason::Monologue),
        "a system entry passes through without counting as a different speaker"
    );
}

#[test]
fn monologue_counts_external_as_its_own_speaker() {
    let history = vec![
        plain_entry(Speaker::External, "one"),
        plain_entry(Speaker::External, "two"),
        plain_entry(Speaker::External, "three"),
    ];

    assert_eq!(
        evaluate_limits(&history, &limits_for_monologue()),
        Some(HaltReason::Monologue)
    );
}

#[test]
fn turn_budget_halts_once_ai_messages_since_the_last_user_message_reach_the_configured_max() {
    let mut history = vec![plain_entry(Speaker::User, "kick things off")];
    for i in 0..30 {
        let speaker = if i % 2 == 0 {
            Speaker::Window("w-a".to_string())
        } else {
            Speaker::External
        };
        history.push(plain_entry(speaker, &format!("update {i}")));
    }

    assert_eq!(
        evaluate_limits(&history, &limits_for_turn_budget()),
        Some(HaltReason::TurnBudget),
        "external counts the same as a window toward the turn budget"
    );
}

#[test]
fn turn_budget_does_not_halt_one_short_of_the_configured_max() {
    let mut history = vec![plain_entry(Speaker::User, "kick things off")];
    for i in 0..29 {
        history.push(plain_entry(
            Speaker::Window("w-a".to_string()),
            &format!("update {i}"),
        ));
    }

    assert_eq!(evaluate_limits(&history, &limits_for_turn_budget()), None);
}

#[test]
fn turn_budget_resets_after_a_user_message() {
    let mut history = vec![plain_entry(Speaker::User, "kick things off")];
    for i in 0..29 {
        history.push(plain_entry(
            Speaker::Window("w-a".to_string()),
            &format!("update {i}"),
        ));
    }
    history.push(plain_entry(Speaker::User, "thanks, one more thing"));
    history.push(plain_entry(Speaker::Window("w-a".to_string()), "sure"));
    history.push(plain_entry(Speaker::External, "on it"));

    assert_eq!(evaluate_limits(&history, &limits_for_turn_budget()), None);
}

// =====================================================================
// The silence candidate rule
//
// No function name is given in the architecture for this rule, so this
// file settles on `is_silence_candidate(check: &SilenceCheck,
// silence_timeout_ms: u64) -> bool`. `SilenceCheck` carries: the delivery
// state recorded for every addressed window (`ChatDeliveryState`), whether
// any of them has spoken since, how long has elapsed since the message was
// posted, whether the room is currently halted, and whether this message
// was already noted as silent.
// =====================================================================

fn silence_check(
    deliveries: Vec<ChatDeliveryState>,
    replied_since: bool,
    elapsed_ms: u64,
) -> SilenceCheck {
    SilenceCheck {
        deliveries,
        replied_since,
        elapsed_ms,
        halted: false,
        silence_noted: false,
    }
}

#[test]
fn a_placed_message_with_no_reply_is_a_candidate_once_the_timeout_elapses() {
    let check = silence_check(vec![ChatDeliveryState::Confirmed], false, 300_000);
    assert!(is_silence_candidate(&check, 300_000));
}

#[test]
fn a_placed_message_is_not_a_candidate_before_the_timeout_elapses() {
    let check = silence_check(vec![ChatDeliveryState::Confirmed], false, 299_999);
    assert!(!is_silence_candidate(&check, 300_000));
}

#[test]
fn the_timeout_boundary_itself_counts_as_elapsed() {
    let check = silence_check(vec![ChatDeliveryState::Unconfirmed], false, 300_000);
    assert!(is_silence_candidate(&check, 300_000));
}

#[test]
fn a_message_an_addressed_window_has_replied_to_is_not_a_candidate() {
    let check = silence_check(vec![ChatDeliveryState::Confirmed], true, 300_000);
    assert!(!is_silence_candidate(&check, 300_000));
}

#[test]
fn a_message_placed_nowhere_is_not_a_candidate() {
    let check = silence_check(
        vec![
            ChatDeliveryState::NotWritten,
            ChatDeliveryState::WriteFailed,
            ChatDeliveryState::UndeliveredHalted,
        ],
        false,
        1_000_000,
    );
    assert!(
        !is_silence_candidate(&check, 300_000),
        "a message with no delivery that actually landed anywhere is not being watched \
         for silence"
    );
}

#[test]
fn a_message_placed_in_at_least_one_window_is_a_candidate_even_if_others_failed() {
    let check = silence_check(
        vec![ChatDeliveryState::NotWritten, ChatDeliveryState::Confirmed],
        false,
        300_000,
    );
    assert!(is_silence_candidate(&check, 300_000));
}

#[test]
fn a_message_is_not_a_candidate_while_the_room_is_halted() {
    let mut check = silence_check(vec![ChatDeliveryState::Confirmed], false, 300_000);
    check.halted = true;
    assert!(!is_silence_candidate(&check, 300_000));
}

#[test]
fn a_message_already_noted_as_silent_is_not_a_candidate_again() {
    let mut check = silence_check(vec![ChatDeliveryState::Confirmed], false, 300_000);
    check.silence_noted = true;
    assert!(!is_silence_candidate(&check, 300_000));
}

#[test]
fn a_message_placed_only_as_not_observed_is_a_candidate() {
    let check = silence_check(vec![ChatDeliveryState::NotObserved], false, 300_000);
    assert!(
        is_silence_candidate(&check, 300_000),
        "not_observed still means the envelope was written; only the window's own reaction \
         went unseen, which is exactly the case this rule watches for"
    );
}

// `ChatDeliveryState` carries the nine delivery states, one wire
// string apiece, and `as_str` / `parse` invert each other; an unrecognized
// string is rejected (fail-closed) rather than guessed at.
#[test]
fn chat_delivery_state_round_trips_through_its_wire_string_for_every_state() {
    let states = [
        (ChatDeliveryState::Pending, "pending"),
        (ChatDeliveryState::Confirmed, "confirmed"),
        (ChatDeliveryState::Unconfirmed, "unconfirmed"),
        (ChatDeliveryState::NotObserved, "not_observed"),
        (ChatDeliveryState::NotWritten, "not_written"),
        (ChatDeliveryState::WriteFailed, "write_failed"),
        (ChatDeliveryState::UndeliveredUnknown, "undelivered_unknown"),
        (ChatDeliveryState::UndeliveredHalted, "undelivered_halted"),
        (
            ChatDeliveryState::UndeliveredExhausted,
            "undelivered_exhausted",
        ),
    ];
    for (state, wire) in states {
        assert_eq!(
            state.as_str(),
            wire,
            "unexpected wire spelling for {state:?}"
        );
        assert_eq!(
            ChatDeliveryState::parse(wire),
            Some(state),
            "parse must invert as_str for {wire:?}"
        );
    }
}

#[test]
fn chat_delivery_state_parse_rejects_an_unrecognized_string() {
    assert_eq!(ChatDeliveryState::parse("delivered"), None);
    assert_eq!(ChatDeliveryState::parse(""), None);
    assert_eq!(ChatDeliveryState::parse("Pending"), None);
}

// =====================================================================
// The recorded delivery outcome: one value, and only one arm may carry a
// reason.
//
// The architecture names the type (`DeliveryOutcome`, shaped like
// `DetectedStatus`) and its projection (`state()`) but not the rest of the
// signature, so this file settles the remainder, called out here so the
// implementation follows it rather than reverse-engineering it from the
// assertions:
//
// - `UndeliveredUnknown(Option<UnknownReason>)` is the only arm carrying
//   data. The other eight arms are plain and share their names with
//   `ChatDeliveryState`.
// - `state(self) -> ChatDeliveryState` and
//   `unknown_reason(self) -> Option<UnknownReason>` are the two projections,
//   mirroring the pair `DetectedStatus` already exposes.
// - `from_parts(state: ChatDeliveryState, reason: Option<UnknownReason>) -> Self`
//   folds a persisted pair back into one value, keeping the reason only on
//   the arm able to hold it.
// - The type derives `Debug, Clone, Copy, PartialEq, Eq`.
// =====================================================================

const ALL_UNKNOWN_REASONS: [UnknownReason; 6] = [
    UnknownReason::NoDefinition,
    UnknownReason::NoRuleMatched,
    UnknownReason::ScreenLost,
    UnknownReason::DetectorStopped,
    UnknownReason::NotYetJudged,
    UnknownReason::DeclaredUnknown,
];

/// Each outcome paired with the delivery state it projects to, covering the
/// whole vocabulary once each. The status-unknown arm appears here in its
/// reason-less form; the reason-carrying forms are enumerated separately.
fn every_outcome() -> Vec<(DeliveryOutcome, ChatDeliveryState)> {
    vec![
        (DeliveryOutcome::Pending, ChatDeliveryState::Pending),
        (DeliveryOutcome::Confirmed, ChatDeliveryState::Confirmed),
        (DeliveryOutcome::Unconfirmed, ChatDeliveryState::Unconfirmed),
        (DeliveryOutcome::NotObserved, ChatDeliveryState::NotObserved),
        (DeliveryOutcome::NotWritten, ChatDeliveryState::NotWritten),
        (DeliveryOutcome::WriteFailed, ChatDeliveryState::WriteFailed),
        (
            DeliveryOutcome::UndeliveredUnknown(None),
            ChatDeliveryState::UndeliveredUnknown,
        ),
        (
            DeliveryOutcome::UndeliveredHalted,
            ChatDeliveryState::UndeliveredHalted,
        ),
        (
            DeliveryOutcome::UndeliveredExhausted,
            ChatDeliveryState::UndeliveredExhausted,
        ),
    ]
}

#[test]
fn every_delivery_outcome_projects_to_its_own_state_and_the_vocabulary_stays_nine_wide() {
    let pairs = every_outcome();
    let projected: Vec<ChatDeliveryState> =
        pairs.iter().map(|(outcome, _)| outcome.state()).collect();
    let expected: Vec<ChatDeliveryState> = pairs.iter().map(|(_, state)| *state).collect();
    assert_eq!(
        projected, expected,
        "each outcome must project to the delivery state of the same name"
    );

    let mut wires: Vec<&str> = projected.iter().map(ChatDeliveryState::as_str).collect();
    wires.sort_unstable();
    wires.dedup();
    assert_eq!(
        wires.len(),
        9,
        "the outcomes must project onto nine distinct wire strings, the same vocabulary \
         the delivery state already spells: {wires:?}"
    );

    for wire in wires {
        assert!(
            ChatDeliveryState::parse(wire).is_some(),
            "every projected wire string must still be one the vocabulary parses: {wire:?}"
        );
    }
}

#[test]
fn only_the_status_unknown_outcome_can_carry_a_reason() {
    for (outcome, state) in every_outcome() {
        if state == ChatDeliveryState::UndeliveredUnknown {
            continue;
        }
        assert_eq!(
            outcome.unknown_reason(),
            None,
            "{outcome:?} is not the status-unknown outcome, so it must carry no reason"
        );
    }

    for reason in ALL_UNKNOWN_REASONS {
        let outcome = DeliveryOutcome::UndeliveredUnknown(Some(reason));
        assert_eq!(
            outcome.unknown_reason(),
            Some(reason),
            "the status-unknown outcome must hand back the reason it was built with"
        );
        assert_eq!(
            outcome.state(),
            ChatDeliveryState::UndeliveredUnknown,
            "carrying a reason must not move the outcome off the status-unknown state"
        );
    }
}

#[test]
fn the_status_unknown_outcome_also_stands_without_a_reason() {
    let bare = DeliveryOutcome::UndeliveredUnknown(None);
    let carried = DeliveryOutcome::UndeliveredUnknown(Some(UnknownReason::ScreenLost));

    assert_eq!(
        bare.unknown_reason(),
        None,
        "a delivery withheld with nothing observed about the destination must be \
         expressible without inventing a reason"
    );
    assert_eq!(
        bare.state(),
        carried.state(),
        "both forms describe the same delivery state"
    );
    assert_ne!(
        bare, carried,
        "the two forms must stay distinguishable as values, even though they project to \
         the same state"
    );
}

#[test]
fn folding_a_persisted_pair_keeps_the_reason_only_where_it_belongs() {
    for (outcome, state) in every_outcome() {
        assert_eq!(
            DeliveryOutcome::from_parts(state, None),
            outcome,
            "a pair with no reason must fold back into the plain outcome for {state:?}"
        );
    }

    for (_, state) in every_outcome() {
        for reason in ALL_UNKNOWN_REASONS {
            let folded = DeliveryOutcome::from_parts(state, Some(reason));
            assert_eq!(
                folded.state(),
                state,
                "folding must never move the pair off its recorded state"
            );
            let kept = if state == ChatDeliveryState::UndeliveredUnknown {
                Some(reason)
            } else {
                None
            };
            assert_eq!(
                folded.unknown_reason(),
                kept,
                "a reason paired with {state:?} must survive only on the status-unknown \
                 state, and be dropped everywhere else"
            );
        }
    }
}

#[test]
fn the_silence_rule_weighs_the_delivery_state_without_its_reason() {
    let bare = DeliveryOutcome::UndeliveredUnknown(None);
    let carried = DeliveryOutcome::UndeliveredUnknown(Some(UnknownReason::ScreenLost));

    let with_bare = silence_check(
        vec![DeliveryOutcome::Confirmed.state(), bare.state()],
        false,
        300_000,
    );
    let with_carried = silence_check(
        vec![DeliveryOutcome::Confirmed.state(), carried.state()],
        false,
        300_000,
    );
    assert_eq!(
        with_bare, with_carried,
        "the evidence the silence rule weighs must be built from the delivery state alone, \
         so two outcomes differing only in their reason produce the same evidence"
    );
    assert!(is_silence_candidate(&with_bare, 300_000));
    assert_eq!(
        is_silence_candidate(&with_bare, 300_000),
        is_silence_candidate(&with_carried, 300_000),
        "a reason recorded next to a delivery must not change the silence verdict"
    );

    let only_bare = silence_check(vec![bare.state()], false, 1_000_000);
    let only_carried = silence_check(vec![carried.state()], false, 1_000_000);
    assert!(
        !is_silence_candidate(&only_bare, 300_000),
        "a message that landed nowhere is not watched for silence"
    );
    assert!(
        !is_silence_candidate(&only_carried, 300_000),
        "recording why the destination's status was unknown must not turn a message that \
         landed nowhere into a silence candidate"
    );
}

// =====================================================================
// The chat envelope
//
// `compose_chat_envelope(from, room_id, mentions, broadcast, body) ->
// Result<String, ChatEnvelopeTooLong>`. The architecture names the
// function and the wire shape but not the `from` type; this file settles
// it as `ChatEnvelopeFrom { Window { window_id, name }, User, External }`,
// mirroring `yagura_daemon::handoff::EnvelopeFrom`. `mentions` carries
// resolved display names (not window ids): the caller looks names up
// through the roster before composing the envelope, so this function
// never needs one.
// =====================================================================

fn parse_envelope_json(prefix: &str, text: &str) -> serde_json::Value {
    let json_part = text
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("envelope must start with {prefix:?}: {text:?}"));
    serde_json::from_str(json_part)
        .unwrap_or_else(|err| panic!("envelope JSON part must parse: {err}, text: {json_part:?}"))
}

const CHAT_PREFIX: &str = "[yagura chat] ";

#[test]
fn send_chat_envelope_matches_the_documented_shape_for_a_window_sender() {
    let from = ChatEnvelopeFrom::Window {
        window_id: "w-a".to_string(),
        name: "Alice".to_string(),
    };
    let envelope = compose_chat_envelope(&from, "main", &["Bob".to_string()], false, "hi")
        .expect("a short envelope must fit under the byte cap");

    assert_eq!(
        envelope,
        "[yagura chat] {\"from\":{\"window_id\":\"w-a\",\"name\":\"Alice\"},\"room_id\":\
         \"main\",\"mentions\":[\"Bob\"],\"broadcast\":false,\"body\":\"hi\",\"reply\":\
         {\"tool\":\"send_chat\",\"room_id\":\"main\"}}"
    );
}

#[test]
fn a_broadcast_envelope_carries_an_empty_mentions_list_and_broadcast_true() {
    let from = ChatEnvelopeFrom::Window {
        window_id: "w-a".to_string(),
        name: "Alice".to_string(),
    };
    let envelope = compose_chat_envelope(&from, "main", &[], true, "hi")
        .expect("a short envelope must fit under the byte cap");
    let value = parse_envelope_json(CHAT_PREFIX, &envelope);

    assert_eq!(value["mentions"], serde_json::json!([]));
    assert_eq!(value["broadcast"], true);
}

#[test]
fn a_user_sender_serializes_as_the_bare_string_user() {
    let envelope = compose_chat_envelope(&ChatEnvelopeFrom::User, "main", &[], true, "hi")
        .expect("a short envelope must fit under the byte cap");
    let value = parse_envelope_json(CHAT_PREFIX, &envelope);

    assert_eq!(value["from"], "user");
}

#[test]
fn an_external_sender_serializes_as_the_bare_string_external() {
    let envelope = compose_chat_envelope(&ChatEnvelopeFrom::External, "main", &[], true, "hi")
        .expect("a short envelope must fit under the byte cap");
    let value = parse_envelope_json(CHAT_PREFIX, &envelope);

    assert_eq!(value["from"], "external");
}

// `reply` carries the room a reply belongs in (parallel to
// `send_handoff`'s `reply.window_id`), so an AI can post back through
// `send_chat` without tracking room membership itself.
#[test]
fn the_reply_object_names_send_chat_and_carries_the_rooms_id() {
    let envelope = compose_chat_envelope(&ChatEnvelopeFrom::User, "main", &[], true, "hi")
        .expect("a short envelope must fit under the byte cap");
    let value = parse_envelope_json(CHAT_PREFIX, &envelope);

    assert_eq!(
        value["reply"],
        serde_json::json!({ "tool": "send_chat", "room_id": "main" })
    );
}

#[test]
fn the_reply_objects_room_id_matches_whatever_room_id_was_passed_in() {
    let envelope = compose_chat_envelope(
        &ChatEnvelopeFrom::User,
        "r-0123456789abcdef",
        &[],
        true,
        "hi",
    )
    .expect("a short envelope must fit under the byte cap");
    let value = parse_envelope_json(CHAT_PREFIX, &envelope);

    assert_eq!(value["room_id"], "r-0123456789abcdef");
    assert_eq!(value["reply"]["room_id"], "r-0123456789abcdef");
}

#[test]
fn a_quote_backslash_and_newline_in_the_body_are_escaped_and_the_envelope_stays_one_line() {
    let body = "she said \"go\\stop\"\nnow";
    let envelope = compose_chat_envelope(&ChatEnvelopeFrom::User, "main", &[], true, body)
        .expect("escaping must not itself exceed the byte cap for this fixture");

    assert!(!envelope.contains('\n'));
    assert!(!envelope.contains('\r'));
    let value = parse_envelope_json(CHAT_PREFIX, &envelope);
    assert_eq!(value["body"], body);
}

#[test]
fn an_envelope_exactly_at_the_byte_cap_is_accepted() {
    let from = ChatEnvelopeFrom::Window {
        window_id: "w-a".to_string(),
        name: "n".to_string(),
    };
    let base = compose_chat_envelope(&from, "main", &[], true, "t")
        .expect("a short envelope must fit under the byte cap");
    let extra = HANDOFF_ENVELOPE_MAX_BYTES - base.len();
    let body = "t".to_string() + &"x".repeat(extra);

    let envelope =
        compose_chat_envelope(&from, "main", &[], true, &body).expect("must fit exactly");
    assert_eq!(envelope.len(), HANDOFF_ENVELOPE_MAX_BYTES);
}

#[test]
fn an_envelope_one_byte_over_the_cap_is_rejected() {
    let from = ChatEnvelopeFrom::Window {
        window_id: "w-a".to_string(),
        name: "n".to_string(),
    };
    let base = compose_chat_envelope(&from, "main", &[], true, "t")
        .expect("a short envelope must fit under the byte cap");
    let target = HANDOFF_ENVELOPE_MAX_BYTES + 1;
    let extra = target - base.len();
    let body = "t".to_string() + &"x".repeat(extra);

    let result = compose_chat_envelope(&from, "main", &[], true, &body);
    match result {
        Err(ChatEnvelopeTooLong { len, max }) => {
            assert_eq!(len, target);
            assert_eq!(max, HANDOFF_ENVELOPE_MAX_BYTES);
        }
        Ok(text) => panic!(
            "expected ChatEnvelopeTooLong, got an envelope of {} bytes",
            text.len()
        ),
    }
}

// =====================================================================
// `[chat]` limits' defaults and range
// =====================================================================

#[test]
fn chat_limits_default_matches_the_documented_values() {
    let limits = ChatLimits::default();

    assert_eq!(limits.repetition_count, 4);
    assert_eq!(limits.repetition_window, 12);
    assert_eq!(limits.repetition_distance, 6);
    assert_eq!(limits.monologue_max, 3);
    assert_eq!(limits.turn_message_max, 30);
    assert_eq!(limits.silence_timeout_secs, 300);
    assert_eq!(limits.history_max, 5_000);
}

#[test]
fn chat_limits_default_passes_its_own_validation() {
    assert_eq!(ChatLimits::default().validate(), Ok(()));
}

#[test]
fn validate_rejects_a_one_or_more_field_set_to_zero() {
    let limits = ChatLimits {
        repetition_count: 0,
        ..ChatLimits::default()
    };
    assert!(limits.validate().is_err());
}

#[test]
fn validate_rejects_a_repetition_distance_past_its_upper_bound() {
    let limits = ChatLimits {
        repetition_distance: 65,
        ..ChatLimits::default()
    };
    assert!(limits.validate().is_err());
}

#[test]
fn validate_accepts_a_repetition_distance_at_either_end_of_its_range() {
    let low = ChatLimits {
        repetition_distance: 0,
        ..ChatLimits::default()
    };
    let high = ChatLimits {
        repetition_distance: 64,
        ..ChatLimits::default()
    };
    assert_eq!(low.validate(), Ok(()));
    assert_eq!(high.validate(), Ok(()));
}

#[test]
fn validate_rejects_a_silence_timeout_past_its_upper_bound() {
    let limits = ChatLimits {
        silence_timeout_secs: 86_401,
        ..ChatLimits::default()
    };
    assert!(limits.validate().is_err());
}

#[test]
fn validate_accepts_a_silence_timeout_at_either_end_of_its_range() {
    let low = ChatLimits {
        silence_timeout_secs: 1,
        ..ChatLimits::default()
    };
    let high = ChatLimits {
        silence_timeout_secs: 86_400,
        ..ChatLimits::default()
    };
    assert_eq!(low.validate(), Ok(()));
    assert_eq!(high.validate(), Ok(()));
}

#[test]
fn validate_error_names_the_field_and_the_range_it_violated() {
    let limits = ChatLimits {
        repetition_distance: 65,
        ..ChatLimits::default()
    };
    let Err(err) = limits.validate() else {
        panic!("expected an out-of-range repetition_distance to be rejected");
    };
    let ChatLimitsError {
        field,
        value,
        min,
        max,
    } = err;
    assert_eq!(field, "repetition_distance");
    assert_eq!(value, 65);
    assert_eq!(min, 0);
    assert_eq!(max, 64);
    assert_eq!(
        err.to_string(),
        format!("{field} must be between {min} and {max}, got {value}")
    );
}
