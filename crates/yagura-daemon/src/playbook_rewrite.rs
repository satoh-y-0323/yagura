//! Turning a destructive tool's auto-approval, or the plan-review
//! requirement, on or off by rewriting the daemon's playbook file in place.
//!
//! The playbook is written by hand, so both operations edit the parsed
//! document rather than re-serialising the policy: every byte outside the
//! edited (or added) value stays as the operator wrote it. Each rewrite is
//! read back through [`parse_policy`] before it replaces the file, and
//! nothing is written when the file's own current value already matches the
//! request or the read-back does not match every other section besides the
//! one requested.
//!
//! These functions decide only whether the *file* needs writing, from the
//! file's own current value; whether the daemon's already-running policy
//! needs updating from a write's outcome is a judgement made elsewhere.

use std::path::{Path, PathBuf};

use thiserror::Error;
use toml_edit::{Document, Item, TableLike, Value};

use crate::config_rewrite;
use crate::control::{ApprovalPolicy, DestructiveTool, PlansSection, parse_policy};

/// The outcome of [`set_auto_approve`] or [`set_plan_approval_required`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybookWrite {
    /// Whether the file was rewritten (`false` when it was already in the
    /// requested state).
    pub written: bool,
}

/// Why a playbook rewrite request was not carried out. Every variant's
/// message is a fixed phrase: none carries the file's path or content, so a
/// caller relaying it to a wire response never leaks either.
#[derive(Debug, Error)]
pub enum PlaybookRewriteError {
    /// The file could not be read, or its contents do not parse as a
    /// playbook (including an unknown key or an out-of-range value).
    #[error("the playbook could not be read")]
    Unreadable,
    /// The edited document did not read back as requested.
    #[error("the playbook could not be rewritten as requested")]
    RewriteFailed,
    /// Replacing the file failed.
    #[error("the playbook could not be written")]
    Io(#[source] std::io::Error),
}

impl From<config_rewrite::RewriteError> for PlaybookRewriteError {
    fn from(err: config_rewrite::RewriteError) -> Self {
        match err {
            config_rewrite::RewriteError::RewriteFailed => Self::RewriteFailed,
            config_rewrite::RewriteError::Io(err) => Self::Io(err),
        }
    }
}

/// Resolves `path` to an absolute one without touching the process's current
/// directory, the same safety [`replace_file`][config_rewrite::replace_file]
/// applies before splitting a path into its excluded parent and its hardened
/// leaf directory. Exposed here so that guarantee can be tested directly.
///
/// # Errors
///
/// Returns any [`std::io::Error`] resolving the path itself returns.
pub fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    config_rewrite::absolute_path(path)
}

/// Turns `tool`'s auto-approval on or off in the playbook at `path`, editing
/// its `[approval]` table (however it is written: a heading, a root-level
/// inline table, or root-level dotted keys) in place. Blocking: reads and
/// replaces a file.
///
/// # Errors
///
/// [`PlaybookRewriteError::Unreadable`] when the file cannot be read or does
/// not parse as a playbook, [`PlaybookRewriteError::RewriteFailed`] when the
/// edited document does not read back with `tool` in the requested state and
/// every other tool and table unchanged, and [`PlaybookRewriteError::Io`]
/// when the file cannot be replaced. On every error the file is left as it
/// was.
pub fn set_auto_approve(
    path: &Path,
    tool: DestructiveTool,
    on: bool,
) -> Result<PlaybookWrite, PlaybookRewriteError> {
    let source = read_source(path)?;
    let before = read_policy(&source)?;
    if before.allows(tool) == on {
        return Ok(PlaybookWrite { written: false });
    }

    let mut document = config_rewrite::parse_document("playbook", &source)?;
    {
        let table = approval_table_mut(&mut document)?;
        let array = auto_approve_array_mut(table)?;
        if on {
            append_matching_sibling(array, tool.as_tool_name());
        } else {
            array.retain(|value| value.as_str() != Some(tool.as_tool_name()));
        }
    }

    let rewritten = config_rewrite::with_line_endings_of(&source, &document.to_string());
    let after = read_rewritten_policy(&rewritten)?;
    if after.allows(tool) != on {
        return Err(PlaybookRewriteError::RewriteFailed);
    }
    for other in DestructiveTool::ALL {
        if other != tool && before.allows(other) != after.allows(other) {
            return Err(PlaybookRewriteError::RewriteFailed);
        }
    }
    if before.plans != after.plans || before.limits != after.limits || before.chat != after.chat {
        return Err(PlaybookRewriteError::RewriteFailed);
    }

    config_rewrite::replace_file("playbook", path, &rewritten)?;
    Ok(PlaybookWrite { written: true })
}

/// Turns whether a new plan waits for the operator's approval on or off in
/// the playbook at `path`, editing its `[plans] require_approval` key (adding
/// the key, or the whole table, when either is absent) in place. Blocking:
/// reads and replaces a file.
///
/// # Errors
///
/// Same as [`set_auto_approve`], with the edited document instead read back
/// for `require_approval` matching the request and every tool's approval
/// unchanged.
pub fn set_plan_approval_required(
    path: &Path,
    required: bool,
) -> Result<PlaybookWrite, PlaybookRewriteError> {
    let source = read_source(path)?;
    let before = read_policy(&source)?;
    if plan_approval_required(&before) == required {
        return Ok(PlaybookWrite { written: false });
    }

    let mut document = config_rewrite::parse_document("playbook", &source)?;
    {
        let table = plans_table_mut(&mut document)?;
        set_require_approval_value(table, required);
    }

    let rewritten = config_rewrite::with_line_endings_of(&source, &document.to_string());
    let after = read_rewritten_policy(&rewritten)?;
    if plan_approval_required(&after) != required {
        return Err(PlaybookRewriteError::RewriteFailed);
    }
    for tool in DestructiveTool::ALL {
        if before.allows(tool) != after.allows(tool) {
            return Err(PlaybookRewriteError::RewriteFailed);
        }
    }
    if before.limits != after.limits || before.chat != after.chat {
        return Err(PlaybookRewriteError::RewriteFailed);
    }

    config_rewrite::replace_file("playbook", path, &rewritten)?;
    Ok(PlaybookWrite { written: true })
}

fn read_source(path: &Path) -> Result<String, PlaybookRewriteError> {
    std::fs::read_to_string(path).map_err(|err| {
        tracing::warn!("failed to read the playbook at {}: {err}", path.display());
        PlaybookRewriteError::Unreadable
    })
}

fn read_policy(source: &str) -> Result<ApprovalPolicy, PlaybookRewriteError> {
    parse_policy(source).map_err(|err| {
        tracing::warn!("failed to parse the playbook for rewriting: {err}");
        PlaybookRewriteError::Unreadable
    })
}

/// Parses the edited-and-rendered document back into a policy, so a bug in
/// the edit itself is reported as [`PlaybookRewriteError::RewriteFailed`]
/// rather than [`PlaybookRewriteError::Unreadable`] (which names the file the
/// caller handed in, not the edit this module produced).
fn read_rewritten_policy(rewritten: &str) -> Result<ApprovalPolicy, PlaybookRewriteError> {
    parse_policy(rewritten).map_err(|err| {
        tracing::warn!("the playbook edit did not produce valid TOML: {err}");
        PlaybookRewriteError::RewriteFailed
    })
}

fn plan_approval_required(policy: &ApprovalPolicy) -> bool {
    PlansSection::requires_approval(policy.plans.as_ref())
}

/// The document's `approval` table in whichever form it is written: a
/// `[approval]` table, a root-level inline table or root-level dotted keys.
/// Creates an empty `[approval]` table first when none of those is present.
fn approval_table_mut(document: &mut Document) -> Result<&mut dyn TableLike, PlaybookRewriteError> {
    if document.get("approval").is_none() {
        document
            .as_table_mut()
            .insert("approval", toml_edit::table());
    }
    document
        .get_mut("approval")
        .and_then(Item::as_table_like_mut)
        .ok_or(PlaybookRewriteError::RewriteFailed)
}

/// The document's `plans` table, the same way [`approval_table_mut`] finds
/// (or creates) `approval`.
fn plans_table_mut(document: &mut Document) -> Result<&mut dyn TableLike, PlaybookRewriteError> {
    if document.get("plans").is_none() {
        document.as_table_mut().insert("plans", toml_edit::table());
    }
    document
        .get_mut("plans")
        .and_then(Item::as_table_like_mut)
        .ok_or(PlaybookRewriteError::RewriteFailed)
}

/// The approval table's `auto_approve` array, creating an empty one first
/// when the key is absent.
fn auto_approve_array_mut(
    table: &mut dyn TableLike,
) -> Result<&mut toml_edit::Array, PlaybookRewriteError> {
    if table.get("auto_approve").is_none() {
        table.insert("auto_approve", toml_edit::value(toml_edit::Array::new()));
    }
    table
        .get_mut("auto_approve")
        .and_then(Item::as_array_mut)
        .ok_or(PlaybookRewriteError::RewriteFailed)
}

/// Appends `tool_name` to `array`, copying the decor (surrounding whitespace)
/// of the array's current last entry onto the new one so a multi-line array
/// gains its own indented element line instead of everything landing on the
/// previous entry's line; an empty array keeps the default single-line
/// formatting `toml_edit` gives a freshly pushed value.
fn append_matching_sibling(array: &mut toml_edit::Array, tool_name: &str) {
    let sibling_decor = array.iter().last().map(|value| value.decor().clone());
    array.push(tool_name);
    if let Some(decor) = sibling_decor {
        let last = array.len() - 1;
        if let Some(value) = array.get_mut(last) {
            *value.decor_mut() = decor;
        }
    }
}

/// Sets `require_approval` to `required`, keeping the existing value's
/// surrounding whitespace and same-line comment, or adding the key (with
/// `toml_edit`'s default formatting) when it is absent.
fn set_require_approval_value(table: &mut dyn TableLike, required: bool) {
    if let Some(value) = table
        .get_mut("require_approval")
        .and_then(Item::as_value_mut)
    {
        let mut replacement = Value::from(required);
        *replacement.decor_mut() = value.decor().clone();
        *value = replacement;
    } else {
        table.insert("require_approval", toml_edit::value(required));
    }
}
