#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_daemon::json_entry`: editing top-level entries of a user's own
//! JSON settings file in place. Each case sets one entry and compares the
//! whole resulting text, so both the scan of the existing document and the
//! layout of the written entry are measured.

use serde_json::{Value, json};
use yagura_daemon::json_entry::set_entries;

fn set_b(source: &str, value: Value) -> String {
    set_entries(source, &[("b".to_string(), value)])
        .unwrap_or_else(|err| panic!("set_entries must accept {source:?}: {err:?}"))
}

#[test]
fn a_new_entry_follows_the_space_indented_lines_of_the_last_one() {
    assert_eq!(
        set_b("{\n  \"a\": 1\n}", json!({ "c": 1 })),
        "{\n  \"a\": 1,\n  \"b\": {\n    \"c\": 1\n  }\n}"
    );
}

#[test]
fn a_new_entry_follows_the_tab_indented_lines_of_the_last_one() {
    assert_eq!(
        set_b("{\n\t\"a\": 1\n}", json!(2)),
        "{\n\t\"a\": 1,\n\t\"b\": 2\n}"
    );
}

#[test]
fn an_entry_after_one_holding_an_escaped_quote_is_replaced_in_place() {
    assert_eq!(
        set_b(r#"{"a": "say \"hi\"", "b": 1}"#, json!(2)),
        r#"{"a": "say \"hi\"", "b": 2}"#
    );
}

#[test]
fn an_entry_after_a_string_holding_a_space_is_replaced_in_place() {
    assert_eq!(
        set_b(r#"{"a": "x y", "b": 1}"#, json!(2)),
        r#"{"a": "x y", "b": 2}"#
    );
}

#[test]
fn an_entry_after_a_nested_string_holding_a_brace_is_replaced_in_place() {
    assert_eq!(
        set_b(r#"{"a": {"s": "}"}, "b": 1}"#, json!(2)),
        r#"{"a": {"s": "}"}, "b": 2}"#
    );
}
