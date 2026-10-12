//! Detection-definition fixtures shared by the daemon and MCP integration
//! tests: the control definition (a rule that drives a window to `idle`), the
//! submit definition, and the `[composer]` table.

use crate::process::{COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR};

/// The one spelling of the control marker; every other form derives from it.
macro_rules! control_ready_marker {
    () => {
        "CTRL-READY"
    };
}

/// File stem of the control definition inside a detection directory, and so
/// the `detection_name` a window resolves.
pub const CONTROL_DETECTION_NAME: &str = "control-test";

/// The text the control definition's `idle` rule matches.
pub const CONTROL_READY_MARKER: &str = control_ready_marker!();

/// [`CONTROL_READY_MARKER`] as a terminal line: what a test writes into a
/// window's input, or a fixture prints, to trigger the `idle` rule.
pub const CONTROL_READY_LINE: &str = concat!(control_ready_marker!(), "\r\n");

/// A definition whose only rule sends `CTRL-READY` to `idle`, so a window
/// under it can leave `Unknown` on demand (`rules = []` never leaves it).
///
/// `head` is placed before `schema_version` (top-level keys such as
/// `bottom_line_count = N\n` must precede every table); `tail` is appended
/// after the rule (`[input]`, `[composer]`, ...). Either may be empty.
#[must_use]
pub fn idle_definition_toml(name: &str, head: &str, tail: &str) -> String {
    format!(
        r#"{head}schema_version = 1
name = "{name}"

[debounce]
confirm_ms = 30

[[rules]]
status = "idle"
pattern = '{CONTROL_READY_MARKER}'
priority = 10
region = "bottom"
{tail}"#
    )
}

/// The `[composer]` table naming the region a fixture in composer-draw mode
/// paints, built from the shared triple in [`crate::process`] so a definition
/// never restates what the fixture prints.
#[must_use]
pub fn composer_section_toml() -> String {
    format!(
        "\n[composer]\nmarker = '{COMPOSER_MARKER}'\nseparator = \"{COMPOSER_SEPARATOR}\"\ncontinuation_prefix = \"{COMPOSER_CONTINUATION_PREFIX}\"\n"
    )
}

/// The definition shape for a `submit_child` window: `marker` (the fixture's
/// startup line) is `waiting_input`, and its `SUBMITTED` / `REACT` output is
/// `running`. `input_toml` is the body written under the `[input]` heading
/// this function writes; no `[composer]` table is appended.
///
/// `marker` goes into a regex pattern verbatim, so it must carry no regex
/// metacharacter.
#[must_use]
pub fn submit_definition_toml(
    name: &str,
    marker: &str,
    bottom_line_count: u16,
    input_toml: &str,
) -> String {
    format!(
        r#"schema_version = 1
name = "{name}"
bottom_line_count = {bottom_line_count}

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = '(SUBMITTED)|(REACT)'
priority = 20
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '{marker}'
priority = 10
region = "bottom"

[input]
{input_toml}
"#
    )
}
