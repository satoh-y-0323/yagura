#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_daemon::json_entry`: editing top-level entries of a user's own
//! JSON settings file in place. Each case sets one entry and compares the
//! whole resulting text, so both the scan of the existing document and the
//! layout of the written entry are measured.

use serde_json::{Value, json};
use yagura_daemon::json_entry::{entries_hold, remove_entries, set_entries};

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

// ---------------------------------------------------------------------
// Entries addressed by a JSON Pointer (RFC 6901): a key starting with `/`.
// ---------------------------------------------------------------------

fn set_at(source: &str, pointer: &str, value: Value) -> Result<String, String> {
    set_entries(source, &[(pointer.to_string(), value)]).map_err(|err| format!("{err:?}"))
}

fn remove_at(source: &str, pointer: &str) -> String {
    remove_entries(source, &[pointer.to_string()])
        .unwrap_or_else(|err| panic!("remove_entries must accept {source:?}: {err:?}"))
}

fn hold_at(source: &str, pointer: &str, value: Value) -> bool {
    entries_hold(source, &[(pointer.to_string(), value)])
        .unwrap_or_else(|err| panic!("entries_hold must accept {source:?}: {err:?}"))
}

fn parsed(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("not JSON: {text:?}: {err}"))
}

#[test]
fn a_pointer_sets_a_nested_entry_and_keeps_every_other_byte() {
    let source = "{\n  \"mcpServers\": {\n    \"other\": {\"x\":  1}\n  },\n  \"k\" :  [1,  2]\n}";
    let result = set_at(source, "/mcpServers/yagura", json!({ "command": "y" })).unwrap();
    assert_eq!(
        parsed(&result),
        json!({
            "mcpServers": { "other": { "x": 1 }, "yagura": { "command": "y" } },
            "k": [1, 2]
        })
    );
    assert!(result.contains("\"other\": {\"x\":  1}"), "{result}");
    assert!(result.ends_with("},\n  \"k\" :  [1,  2]\n}"), "{result}");
}

#[test]
fn a_pointer_replaces_an_existing_nested_entry_in_place() {
    let source = r#"{"a": 1, "m": {"s": {"old": true},  "t": 2}, "z": 3}"#;
    assert_eq!(
        set_at(source, "/m/s", json!(7)).unwrap(),
        r#"{"a": 1, "m": {"s": 7,  "t": 2}, "z": 3}"#
    );
}

#[test]
fn a_pointer_creates_the_missing_parents() {
    let source = "{\"a\":  1,\n  \"z\" : [1,  2]}";
    let result = set_at(source, "/mcpServers/yagura", json!({ "command": "y" })).unwrap();
    assert!(
        result.starts_with("{\"a\":  1,\n  \"z\" : [1,  2]"),
        "{result}"
    );
    assert_eq!(
        parsed(&result),
        json!({ "a": 1, "z": [1, 2], "mcpServers": { "yagura": { "command": "y" } } })
    );
}

#[test]
fn a_pointer_creates_every_missing_level_of_an_empty_document() {
    let result = set_at("{}", "/a/b/c", json!(1)).unwrap();
    assert_eq!(parsed(&result), json!({ "a": { "b": { "c": 1 } } }));
}

#[test]
fn two_pointers_into_one_new_parent_both_land() {
    let result = set_entries(
        "{\"k\": 1}",
        &[
            ("/m/a".to_string(), json!(1)),
            ("/m/b".to_string(), json!(2)),
        ],
    )
    .unwrap();
    assert_eq!(parsed(&result), json!({ "k": 1, "m": { "a": 1, "b": 2 } }));
}

#[test]
fn pointer_escapes_name_keys_holding_a_slash_or_a_tilde() {
    let result = set_at("{}", "/a~1b/c~0d", json!(1)).unwrap();
    assert_eq!(parsed(&result), json!({ "a/b": { "c~d": 1 } }));
    assert!(hold_at(&result, "/a~1b/c~0d", json!(1)));
    assert!(!hold_at(&result, "/a/b/c~0d", json!(1)));
    assert_eq!(
        parsed(&remove_at(&result, "/a~1b/c~0d")),
        json!({ "a/b": {} })
    );
}

#[test]
fn a_pointer_holds_only_when_the_nested_value_is_exactly_the_given_one() {
    let source = r#"{"m": {"s": {"command": "y"}}}"#;
    assert!(hold_at(source, "/m/s", json!({ "command": "y" })));
    assert!(!hold_at(source, "/m/s", json!({ "command": "z" })));
    assert!(!hold_at(source, "/m/missing", json!(1)));
    assert!(!hold_at(source, "/missing/s", json!(1)));
    assert!(!hold_at(source, "/m/s/command/deeper", json!(1)));
}

#[test]
fn removing_a_pointer_takes_only_that_entry_and_keeps_the_emptied_parent() {
    let sibling = r#"{"a":  1, "m": {"yagura": {"c": 1}, "other":  [1]}, "z": 3}"#;
    assert_eq!(
        remove_at(sibling, "/m/yagura"),
        r#"{"a":  1, "m": {"other":  [1]}, "z": 3}"#
    );

    let only = "{\n  \"a\":  1,\n  \"m\": {\n    \"yagura\": {\"c\": 1}\n  }\n}";
    let result = remove_at(only, "/m/yagura");
    assert_eq!(parsed(&result), json!({ "a": 1, "m": {} }));
    assert!(
        result.starts_with("{\n  \"a\":  1,\n  \"m\": {"),
        "{result}"
    );
}

#[test]
fn removing_a_pointer_that_is_not_there_changes_nothing() {
    let source = "{\"a\":  1,\n \"m\": {\"s\": 2}}";
    assert_eq!(remove_at(source, "/m/none"), source);
    assert_eq!(remove_at(source, "/none/deeper"), source);
}

#[test]
fn a_top_level_name_goes_the_same_way_as_a_one_level_pointer() {
    let source = "{\n  \"a\": 1,\n  \"b\": 2\n}";
    assert_eq!(
        set_at(source, "/b", json!(9)).unwrap(),
        set_b(source, json!(9))
    );
    assert_eq!(
        set_at(source, "/c", json!(9)).unwrap(),
        set_entries(source, &[("c".to_string(), json!(9))]).unwrap()
    );
    assert_eq!(
        remove_at(source, "/b"),
        remove_entries(source, &["b".to_string()]).unwrap()
    );
}

#[test]
fn a_name_without_a_leading_slash_stays_a_literal_top_level_key() {
    let result = set_at("{}", "a/b", json!(1)).unwrap();
    assert_eq!(parsed(&result), json!({ "a/b": 1 }));
}

#[test]
fn a_parent_that_is_not_an_object_fails_instead_of_being_rewritten() {
    for source in [
        r#"{"m": []}"#,
        r#"{"m": "text"}"#,
        r#"{"m": 5}"#,
        r#"{"m": null}"#,
        r#"{"m": {"s": [1]}}"#,
    ] {
        let pointer = if source.contains("\"s\"") {
            "/m/s/deeper"
        } else {
            "/m/yagura"
        };
        assert!(
            set_at(source, pointer, json!(1)).is_err(),
            "{pointer} into {source} must fail"
        );
    }
}
