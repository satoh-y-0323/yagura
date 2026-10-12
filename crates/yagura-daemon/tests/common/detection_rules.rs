//! Shared helpers for mutating and judging a detection definition's `[[rules]]` table
//! against fixed screen fixtures, used by every detector-definition integration test
//! (`detector_codex_cli.rs`, `detector_claude_code.rs`, `detector_definition.rs`).

use toml::Value;

use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::definition::{Rule, parse_definition};
use yagura_protocol::WindowStatus;

use super::{ScreenFeed, new_engine_sized, repaint_lines, tick_after_ms};

/// Parses `source` as TOML, applies `mutate` to the `[[rules]]` array in place, and
/// re-serializes. Mutating through `toml::Value` -- never a string substitution -- means
/// each mutation test exercises the loader's actual TOML semantics rather than a
/// coincidence of the source's exact text.
pub fn mutate_rules(source: &str, mutate: impl FnOnce(&mut Vec<Value>)) -> String {
    let mut value: Value = source.parse().expect("the definition must be valid TOML");
    let rules = value
        .as_table_mut()
        .expect("a TOML document is a table at its root")
        .get_mut("rules")
        .expect("the definition must declare [[rules]]")
        .as_array_mut()
        .expect("rules must be a TOML array");
    mutate(rules);
    toml::to_string(&value).expect("a mutated definition must still serialize")
}

pub fn rule_status(rule: &Value) -> Option<&str> {
    rule.as_table()?.get("status")?.as_str()
}

pub fn rule_priority(rule: &Value) -> Option<i64> {
    rule.as_table()?.get("priority")?.as_integer()
}

pub fn rule_priority_for_status(rules: &[Value], status: &str) -> Option<i64> {
    rules
        .iter()
        .find(|rule| rule_status(rule) == Some(status))
        .and_then(rule_priority)
}

pub fn set_rule_priority(rule: &mut Value, priority: i64) {
    rule.as_table_mut()
        .expect("each [[rules]] entry is a table")
        .insert("priority".to_string(), Value::Integer(priority));
}

pub fn set_rule_pattern(rule: &mut Value, pattern: &str) {
    rule.as_table_mut()
        .expect("each [[rules]] entry is a table")
        .insert("pattern".to_string(), Value::String(pattern.to_string()));
}

/// Parses `source` and returns the [`Rule`] that reports `status`, panicking with the
/// status included in the message if the definition declares none.
pub fn rule_for_status(source: &str, status: WindowStatus) -> Rule {
    parse_definition(source)
        .expect("the definition must parse")
        .rules
        .into_iter()
        .find(|rule| rule.status == status)
        .unwrap_or_else(|| panic!("the definition must declare a {status:?} rule"))
}

/// The `[[rules]]` table's own `status` string for `status`, read from `WindowStatus`'s
/// own `Serialize` impl (`serde(rename_all = "snake_case")`) instead of a hand-written
/// table that would silently drift from it on a variant rename or addition.
fn status_toml_value(status: WindowStatus) -> String {
    match serde_json::to_value(status).expect("WindowStatus must serialize") {
        serde_json::Value::String(key) => key,
        other => panic!("WindowStatus must serialize to a JSON string, got {other:?}"),
    }
}

/// Replaces the pattern of the `[[rules]]` entry reporting `status`, leaving every other
/// rule untouched.
pub fn with_status_pattern(source: &str, status: WindowStatus, pattern: &str) -> String {
    let key = status_toml_value(status);
    mutate_rules(source, |rules| {
        for rule in rules.iter_mut() {
            if rule_status(rule) == Some(key.as_str()) {
                set_rule_pattern(rule, pattern);
            }
        }
    })
}

/// Replaces every `[[rules]]` entry's pattern with `pattern` at once.
pub fn with_every_pattern(source: &str, pattern: &str) -> String {
    mutate_rules(source, |rules| {
        for rule in rules.iter_mut() {
            set_rule_pattern(rule, pattern);
        }
    })
}

/// Feeds `lines` as a single repaint against `definition_toml` at `(cols, rows)` and
/// returns the status confirmed after a tick that exceeds every bundled definition's
/// `confirm_ms` (each definition keeps its own value).
pub async fn judge(definition_toml: &str, cols: u16, rows: u16, lines: &[&str]) -> WindowStatus {
    let config = DaemonConfig::default();
    let mut engine = new_engine_sized(definition_toml, &config, cols, rows);
    engine.feed(&repaint_lines(lines));
    tick_after_ms(&mut engine, 350).await;
    engine.status()
}

/// Feeds `lines` as a single repaint against `definition_toml` at `(cols, rows)`, then
/// advances the paused clock across two separate ticks (`first_stage_ms`, then
/// `second_stage_ms`) before reading the confirmed status.
///
/// A single tick sized `first_stage_ms + second_stage_ms` cannot observe the same
/// thing a caller usually wants from this: `DetectionEngine::evaluate` re-derives the
/// raw judgement on every tick and resets its own debounce clock (`raw_since`) to that
/// tick's own instant the moment the raw judgement changes, so a raw flip that only
/// becomes true partway through one giant advance still needs a *further* wait, past
/// that instant, before it confirms -- a wait a single tick cannot provide since its
/// own `now` is both when the flip is noticed and when the elapsed-time check runs.
/// Splitting the wait in two forces that further wait to actually happen, so a
/// would-be confirmation is caught even if it only becomes possible partway through
/// `first_stage_ms`.
pub async fn judge_after_two_stage_tick(
    definition_toml: &str,
    cols: u16,
    rows: u16,
    lines: &[&str],
    first_stage_ms: u64,
    second_stage_ms: u64,
) -> WindowStatus {
    let config = DaemonConfig::default();
    let mut engine = new_engine_sized(definition_toml, &config, cols, rows);
    engine.feed(&repaint_lines(lines));
    tick_after_ms(&mut engine, first_stage_ms).await;
    tick_after_ms(&mut engine, second_stage_ms).await;
    engine.status()
}
