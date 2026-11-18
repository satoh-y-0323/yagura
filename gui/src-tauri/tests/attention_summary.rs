//! Integration test: the attention decision behind `set_attention_summary`.
//! The state is driven without a window: each observation takes the waiting
//! counts, whether something newly arrived and whether the main window has
//! focus, and returns the title to set and whether to request the user's
//! attention.

use yagura_gui::attention::AttentionState;

const UNFOCUSED: bool = false;
const FOCUSED: bool = true;
const NOT_ARRIVED: bool = false;
const ARRIVED: bool = true;

#[test]
fn the_title_follows_the_sum_of_the_counts_and_nothing_arrived_requests_nothing() {
    let state = AttentionState::new("yagura", Some("dev"));

    let first = state.observe(2, 1, NOT_ARRIVED, UNFOCUSED);
    assert_eq!(first.title, "(3) yagura [dev]");
    assert!(!first.request_attention);

    // The same counts again: the title is rebuilt from the startup stem.
    let again = state.observe(2, 1, NOT_ARRIVED, UNFOCUSED);
    assert_eq!(again.title, "(3) yagura [dev]");

    // Moving a count between the two sources keeps the sum.
    let moved = state.observe(0, 3, NOT_ARRIVED, UNFOCUSED);
    assert_eq!(moved.title, "(3) yagura [dev]");

    // A rising total alone is not an arrival.
    let risen = state.observe(5, 3, NOT_ARRIVED, UNFOCUSED);
    assert_eq!(risen.title, "(8) yagura [dev]");
    assert!(!risen.request_attention);

    let cleared = state.observe(0, 0, NOT_ARRIVED, UNFOCUSED);
    assert_eq!(cleared.title, "yagura [dev]");
    assert!(!cleared.request_attention);
}

#[test]
fn an_arrival_while_the_main_window_lacks_focus_requests_attention() {
    let state = AttentionState::new("yagura", None);

    // The very first observation is not held back.
    let first = state.observe(1, 0, ARRIVED, UNFOCUSED);
    assert!(first.request_attention);
    assert_eq!(first.title, "(1) yagura");

    // A new arrival requests again even when the total did not rise.
    let replaced = state.observe(0, 1, ARRIVED, UNFOCUSED);
    assert!(replaced.request_attention);
    assert_eq!(replaced.title, "(1) yagura");
}

#[test]
fn an_arrival_while_the_main_window_has_focus_requests_nothing() {
    let state = AttentionState::new("yagura", None);

    let focused = state.observe(2, 1, ARRIVED, FOCUSED);
    assert!(!focused.request_attention);
    assert_eq!(focused.title, "(3) yagura");

    // Focus is read per observation: the same arrival without focus requests.
    let unfocused = state.observe(2, 1, ARRIVED, UNFOCUSED);
    assert!(unfocused.request_attention);
}
