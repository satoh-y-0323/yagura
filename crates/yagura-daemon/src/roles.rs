//! The role -> assignment table (`roles.toml`): read/save and the mapping
//! into a [`crate::plan::Assignment`] a caller resolves a task's role name
//! against.
//!
//! `read_roles` and `save_roles` share the same row-shape rules (schema,
//! token shape, duplicate names, launch-option-value shape, the
//! [`MAX_ROLE_ROWS`] limit); only `save_roles` also checks that `profile`
//! names a profile that actually exists, since a reader must not go down
//! because one row references a profile that was since removed.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use yagura_protocol::{is_valid_token, validate_launch_option_value};

use crate::plan::Assignment;

/// Maximum number of `[[role]]` rows a table may declare.
pub const MAX_ROLE_ROWS: usize = 64;

/// Schema version [`read_roles`] / [`save_roles`] accept.
const SUPPORTED_SCHEMA: u32 = 1;

/// One `[[role]]` row: a role name mapped to a launch profile and,
/// optionally, a model/effort override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRow {
    /// Role name, validated against [`is_valid_token`].
    pub name: String,
    /// Launch profile id this role resolves to.
    pub profile: String,
    /// Optional model override, validated against
    /// [`validate_launch_option_value`].
    pub model: Option<String>,
    /// Optional effort override, same validation as `model`.
    pub effort: Option<String>,
}

/// A validated `roles.toml` document: every row's name is unique and passed
/// shape validation, and `default_role` (when set) names one of the rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolesTable {
    /// Rows in document order.
    pub rows: Vec<RoleRow>,
    /// The row a task falls back to when it names no role, or names one this
    /// table has no row for.
    pub default_role: Option<String>,
}

impl RolesTable {
    /// Resolves `role_name` against this table's rows, in the
    /// [`Assignment`] shape [`crate::plan::resolve_assignment`] takes as its
    /// `role` argument.
    ///
    /// Returns `None` when no row's `name` matches `role_name`; the caller
    /// treats an absent role the same way as a table that failed to load.
    #[must_use]
    pub fn assignment_for(&self, role_name: &str) -> Option<Assignment> {
        self.rows
            .iter()
            .find(|row| row.name == role_name)
            .map(|row| Assignment {
                profile: row.profile.clone(),
                model: row.model.clone(),
                effort: row.effort.clone(),
            })
    }

    /// Looks up a task's `role` in the shape [`crate::plan::resolve_assignment`]
    /// takes as its `role` and `default` arguments: the role's row when the
    /// table has one, otherwise the `default_role` row (if any).
    #[must_use]
    pub fn role_and_default_for(
        &self,
        role: Option<&str>,
    ) -> (Option<Assignment>, Option<Assignment>) {
        if let Some(assignment) = role.and_then(|name| self.assignment_for(name)) {
            return (Some(assignment), None);
        }
        let default = self
            .default_role
            .as_deref()
            .and_then(|name| self.assignment_for(name));
        (None, default)
    }
}

/// Why [`read_roles`] or [`save_roles`] rejected a `roles.toml` document.
///
/// Every variant's [`std::fmt::Display`] is a fixed message: a rejected
/// row's name, profile, model or effort value is never echoed back (the
/// same contract [`crate::plan::PlanError`] holds for a plan document).
#[derive(Debug, Error)]
pub enum RolesError {
    /// The file could not be read, or (on save) the replacement could not
    /// be written.
    #[error("roles.toml could not be read or written")]
    Io(#[source] std::io::Error),
    /// The document failed to parse as TOML, or carries an unknown key
    /// (`#[serde(deny_unknown_fields)]`).
    #[error("roles.toml could not be parsed")]
    Toml(#[source] toml::de::Error),
    /// `schema` is not [`SUPPORTED_SCHEMA`].
    #[error("unsupported schema version")]
    UnsupportedSchema,
    /// A row's `name` failed [`is_valid_token`].
    #[error("role name is invalid")]
    InvalidName,
    /// Two rows share the same `name`.
    #[error("role name is duplicated")]
    DuplicateName,
    /// A row's `model` or `effort` failed [`validate_launch_option_value`].
    #[error("model or effort value is invalid")]
    InvalidLaunchOptionValue,
    /// The table declares more than [`MAX_ROLE_ROWS`] rows.
    #[error("table declares {actual} rows, exceeding the {max}-row limit")]
    TooManyRows {
        /// The limit that was exceeded ([`MAX_ROLE_ROWS`]).
        max: usize,
        /// The table's actual row count.
        actual: usize,
    },
    /// (save only) A row's `profile` does not satisfy the caller's
    /// `profile_exists` predicate.
    #[error("profile does not exist")]
    UnknownProfile,
    /// `default_role` does not name one of the table's own rows. Rejected on
    /// read as well as on save, since it is an inconsistency inside the
    /// document rather than drift from another store.
    #[error("default role does not name a role in the table")]
    UnknownDefaultRole,
}

/// Pre-validation representation of a `roles.toml` document. Structural
/// rejection (a missing `schema`, or an unknown key anywhere in the
/// document) happens here, through `serde`/`toml`, and surfaces as
/// [`RolesError::Toml`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawRolesTable {
    schema: u32,
    // Omitted when absent so a build that predates the key still reads the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_role: Option<String>,
    #[serde(default, rename = "role")]
    rows: Vec<RawRoleRow>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawRoleRow {
    name: String,
    profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
}

/// Validates `raw`'s rows against the shape rules shared by [`read_roles`]
/// and [`save_roles`], additionally checking `profile_exists` when it is
/// given (save only).
///
/// Every violation stops validation immediately: a single bad row takes the
/// whole table down rather than being dropped while the rest are kept.
fn validate_rows(
    raw: &RawRolesTable,
    profile_exists: Option<&dyn Fn(&str) -> bool>,
) -> Result<Vec<RoleRow>, RolesError> {
    if raw.schema != SUPPORTED_SCHEMA {
        return Err(RolesError::UnsupportedSchema);
    }
    if raw.rows.len() > MAX_ROLE_ROWS {
        return Err(RolesError::TooManyRows {
            max: MAX_ROLE_ROWS,
            actual: raw.rows.len(),
        });
    }

    let mut seen_names: HashSet<&str> = HashSet::with_capacity(raw.rows.len());
    let mut rows = Vec::with_capacity(raw.rows.len());
    for row in &raw.rows {
        if !is_valid_token(&row.name) {
            return Err(RolesError::InvalidName);
        }
        if !seen_names.insert(row.name.as_str()) {
            return Err(RolesError::DuplicateName);
        }
        if let Some(model) = &row.model
            && validate_launch_option_value(model).is_err()
        {
            return Err(RolesError::InvalidLaunchOptionValue);
        }
        if let Some(effort) = &row.effort
            && validate_launch_option_value(effort).is_err()
        {
            return Err(RolesError::InvalidLaunchOptionValue);
        }
        if let Some(exists) = profile_exists
            && !exists(&row.profile)
        {
            return Err(RolesError::UnknownProfile);
        }
        rows.push(RoleRow {
            name: row.name.clone(),
            profile: row.profile.clone(),
            model: row.model.clone(),
            effort: row.effort.clone(),
        });
    }
    if let Some(default_role) = raw.default_role.as_deref()
        && !rows.iter().any(|row| row.name == default_role)
    {
        return Err(RolesError::UnknownDefaultRole);
    }
    Ok(rows)
}

/// Reads and validates `roles.toml` at `path`.
///
/// A missing file reads as an empty table rather than an error: `roles.toml`
/// is never seeded, so "not configured yet" must not be
/// indistinguishable from "misconfigured". Profile existence is never
/// checked here -- a single row referencing a profile that has since been
/// removed must not take the whole table down; the caller resolves that at
/// use time instead.
///
/// # Errors
///
/// Returns [`RolesError::Io`] for a read failure other than the file being
/// absent, [`RolesError::Toml`] for malformed TOML or an unknown key,
/// [`RolesError::UnsupportedSchema`] for a `schema` other than `1`, and
/// [`RolesError::InvalidName`] / [`RolesError::DuplicateName`] /
/// [`RolesError::InvalidLaunchOptionValue`] / [`RolesError::TooManyRows`]
/// for a row failing shape validation, and [`RolesError::UnknownDefaultRole`]
/// when `default_role` names no row. Any single violation rejects the whole
/// table.
pub fn read_roles(path: &Path) -> Result<RolesTable, RolesError> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RolesTable {
                rows: Vec::new(),
                default_role: None,
            });
        }
        Err(err) => return Err(RolesError::Io(err)),
    };
    let raw: RawRolesTable = toml::from_str(&content).map_err(RolesError::Toml)?;
    let rows = validate_rows(&raw, None)?;
    Ok(RolesTable {
        rows,
        default_role: raw.default_role,
    })
}

/// Validates `table` and, only if every row passes, writes it to `path`
/// through [`yagura_os::replace_config_file`].
///
/// Validation runs first and atomically: a rejected table never touches the
/// file on disk. Unlike [`read_roles`], `profile_exists` is checked for
/// every row's `profile`, since a save is the point at which the operator
/// (through the GUI) is choosing a profile, not merely reading a table that
/// may have drifted since it was written.
///
/// The directory holding `path` is hardened the same way
/// [`yagura_os::seed_config_file`] hardens the directory it seeds into: the
/// directory containing `path` becomes the [`yagura_os::replace_config_file`]
/// `leaf_dir`, and its parent becomes `base_dir`, so this reproduces exactly
/// the `<config base>/<namespace>` boundary `resolve_roles_path` composes.
///
/// # Errors
///
/// Returns the same shape-validation errors [`read_roles`] does, plus
/// [`RolesError::UnknownProfile`] when a row's `profile` fails
/// `profile_exists`, and [`RolesError::Io`] when the replacement write
/// fails.
pub fn save_roles(
    path: &Path,
    table: &RolesTable,
    profile_exists: impl Fn(&str) -> bool,
) -> Result<(), RolesError> {
    let raw = RawRolesTable {
        schema: SUPPORTED_SCHEMA,
        default_role: table.default_role.clone(),
        rows: table
            .rows
            .iter()
            .map(|row| RawRoleRow {
                name: row.name.clone(),
                profile: row.profile.clone(),
                model: row.model.clone(),
                effort: row.effort.clone(),
            })
            .collect(),
    };
    validate_rows(&raw, Some(&profile_exists))?;

    let content =
        toml::to_string(&raw).map_err(|err| RolesError::Io(std::io::Error::other(err)))?;

    let leaf_dir = path.parent().unwrap_or(path);
    let base_dir = leaf_dir.parent().unwrap_or(leaf_dir);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("roles.toml");
    yagura_os::replace_config_file(base_dir, leaf_dir, file_name, &content).map_err(RolesError::Io)
}
