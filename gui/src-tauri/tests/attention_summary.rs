//! Integration test: the attention decision behind `set_attention_summary`.
//! The state is driven without a window: each observation takes the waiting
//! counts, the `baseline` flag and whether the main window has focus, and
//! returns the title to set and whether to request the user's attention.

use yagura_gui::attention::AttentionState;

const UNFOCUSED: bool = false;
const FOCUSED: bool = true;
const NO_BASELINE: bool = false;
const BASELINE: bool = true;

#[test]
fn the_first_observation_only_records_the_total_and_titles_it() {
    let mut state = AttentionState::new("yagura", Some("dev"));

    let update = state.observe(2, 1, NO_BASELINE, UNFOCUSED);

    assert!(!update.request_attention);
    assert_eq!(update.title, "(3) yagura [dev]");
}

#[test]
fn a_rise_while_unfocused_requests_attention_once_and_titles_from_the_startup_stem() {
    let mut state = AttentionState::new("yagura", Some("dev"));
    state.observe(3, 0, NO_BASELINE, UNFOCUSED);

    let risen = state.observe(3, 1, NO_BASELINE, UNFOCUSED);
    assert!(risen.request_attention);
    assert_eq!(risen.title, "(4) yagura [dev]");

    let same = state.observe(3, 1, NO_BASELINE, UNFOCUSED);
    assert!(!same.request_attention);
    assert_eq!(same.title, "(4) yagura [dev]");

    let fallen = state.observe(1, 0, NO_BASELINE, UNFOCUSED);
    assert!(!fallen.request_attention);
    assert_eq!(fallen.title, "(1) yagura [dev]");

    let risen_again = state.observe(2, 0, NO_BASELINE, UNFOCUSED);
    assert!(risen_again.request_attention);

    let cleared = state.observe(0, 0, NO_BASELINE, UNFOCUSED);
    assert!(!cleared.request_attention);
    assert_eq!(cleared.title, "yagura [dev]");
}

#[test]
fn the_total_is_the_sum_of_windows_needing_you_and_pending_permissions() {
    let mut state = AttentionState::new("yagura", None);
    state.observe(1, 0, NO_BASELINE, UNFOCUSED);

    let moved = state.observe(0, 1, NO_BASELINE, UNFOCUSED);
    assert!(!moved.request_attention);
    assert_eq!(moved.title, "(1) yagura");

    let permission_arrived = state.observe(0, 2, NO_BASELINE, UNFOCUSED);
    assert!(permission_arrived.request_attention);
    assert_eq!(permission_arrived.title, "(2) yagura");
}

#[test]
fn a_rise_while_the_main_window_has_focus_requests_nothing() {
    let mut state = AttentionState::new("yagura", None);
    state.observe(0, 0, NO_BASELINE, FOCUSED);

    let risen = state.observe(2, 1, NO_BASELINE, FOCUSED);

    assert!(!risen.request_attention);
    assert_eq!(risen.title, "(3) yagura");

    let lost_focus = state.observe(2, 1, NO_BASELINE, UNFOCUSED);
    assert!(!lost_focus.request_attention);
}

#[test]
fn a_baseline_reset_makes_the_next_observation_only_record_the_total() {
    let mut state = AttentionState::new("yagura", None);
    state.observe(1, 0, NO_BASELINE, UNFOCUSED);

    let reset = state.observe(0, 0, BASELINE, UNFOCUSED);
    assert!(!reset.request_attention);

    // Reconnecting delivers the whole window list at once.
    let reloaded = state.observe(5, 0, NO_BASELINE, UNFOCUSED);
    assert!(!reloaded.request_attention);
    assert_eq!(reloaded.title, "(5) yagura");

    let risen = state.observe(6, 0, NO_BASELINE, UNFOCUSED);
    assert!(risen.request_attention);
}
