#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `roles::read_roles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: `yagura_daemon::roles`, the read/save contract
//! for the role -> assignment table (`roles.toml`).
//!
//! Public surface under test:
//! - `RoleRow { name, profile, model: Option<String>, effort: Option<String> }`
//! - `RolesTable { rows: Vec<RoleRow> }`, with `RolesTable::assignment_for(&self,
//!   role_name: &str) -> Option<yagura_daemon::plan::Assignment>`
//! - `MAX_ROLE_ROWS: usize` (the 64-row limit)
//! - `read_roles(path: &Path) -> Result<RolesTable, RolesError>` (no
//!   `profile_exists` parameter: reading never checks profile existence)
//! - `save_roles(path: &Path, table: &RolesTable, profile_exists: impl Fn(&str)
//!   -> bool) -> Result<(), RolesError>`
//!
//! `read_roles` and `save_roles` share the same shape rules (schema, token
//! shape, duplicate names, launch-option-value shape, row limit); only
//! `save_roles` also checks `profile_exists`.

use std::path::Path;

use yagura_daemon::plan::Assignment;
use yagura_daemon::roles::{
    MAX_ROLE_ROWS, RoleRow, RolesError, RolesTable, read_roles, save_roles,
};
use yagura_test_support::paths::TempDir;

fn roles_path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("roles.toml")
}

fn write(path: &Path, content: &str) {
    std::fs::write(path, content).expect("fixture roles.toml should be writable");
}

fn valid_toml() -> &'static str {
    "schema = 1\n\
     \n\
     [[role]]\n\
     name = \"impl\"\n\
     profile = \"codex-cli\"\n\
     model = \"default\"\n\
     effort = \"high\"\n\
     \n\
     [[role]]\n\
     name = \"review\"\n\
     profile = \"claude-code\"\n"
}

/// `schema = 1` followed by `n` rows named `role-0`..`role-{n-1}`, each with
/// a distinct name and no `model` / `effort`.
fn toml_with_n_distinct_rows(n: usize) -> String {
    let mut out = String::from("schema = 1\n");
    for i in 0..n {
        out.push_str(&format!(
            "\n[[role]]\nname = \"role-{i}\"\nprofile = \"codex-cli\"\n"
        ));
    }
    out
}

fn rows_with_n_distinct_entries(n: usize) -> Vec<RoleRow> {
    (0..n)
        .map(|i| RoleRow {
            name: format!("role-{i}"),
            profile: "codex-cli".to_string(),
            model: None,
            effort: None,
        })
        .collect()
}

// ---------------------------------------------------------------------
// read_roles
// ---------------------------------------------------------------------

#[test]
fn read_roles_returns_an_empty_table_when_the_file_is_absent() {
    let dir = TempDir::new("yagura-roles-test", "absent");
    let table =
        read_roles(&roles_path(&dir)).expect("a missing roles.toml must read as an empty table");
    assert_eq!(
        table.rows.len(),
        0,
        "an absent roles.toml must read as an empty table, not an error"
    );
}

#[test]
fn read_roles_reports_a_file_it_cannot_read_instead_of_an_empty_table() {
    let dir = TempDir::new("yagura-roles-test", "unreadable");
    let path = roles_path(&dir);
    // Not UTF-8: the file is there, but reading it as text fails.
    std::fs::write(&path, [0xff, 0xfe, 0xfd]).expect("fixture roles.toml should be writable");

    assert!(
        matches!(read_roles(&path), Err(RolesError::Io(_))),
        "a roles.toml that exists but cannot be read must be an I/O error, not an empty table"
    );
}

#[test]
fn read_roles_parses_a_valid_table() {
    let dir = TempDir::new("yagura-roles-test", "valid");
    let path = roles_path(&dir);
    write(&path, valid_toml());

    let table = read_roles(&path).expect("a well-formed roles.toml must parse");
    assert_eq!(table.rows.len(), 2);
    assert_eq!(
        table.rows[0],
        RoleRow {
            name: "impl".to_string(),
            profile: "codex-cli".to_string(),
            model: Some("default".to_string()),
            effort: Some("high".to_string()),
        }
    );
    assert_eq!(
        table.rows[1],
        RoleRow {
            name: "review".to_string(),
            profile: "claude-code".to_string(),
            model: None,
            effort: None,
        },
        "model and effort must both stay None when the row omits them"
    );
}

#[test]
fn read_roles_does_not_check_profile_existence() {
    let dir = TempDir::new("yagura-roles-test", "unknown-profile");
    let path = roles_path(&dir);
    write(
        &path,
        "schema = 1\n\n[[role]]\nname = \"impl\"\nprofile = \"no-such-profile-anywhere\"\n",
    );

    let table = read_roles(&path);
    assert!(
        table.is_ok(),
        "read_roles must not check whether `profile` names a profile that actually exists; \
         a single missing profile must not take the whole table down; got {table:?}"
    );
}

#[test]
fn read_roles_rejects_an_unsupported_schema_version() {
    let dir = TempDir::new("yagura-roles-test", "bad-schema");
    let path = roles_path(&dir);
    write(
        &path,
        "schema = 2\n\n[[role]]\nname = \"impl\"\nprofile = \"codex-cli\"\n",
    );

    assert!(
        read_roles(&path).is_err(),
        "an unsupported schema version must be rejected"
    );
}

#[test]
fn read_roles_rejects_an_unknown_key() {
    let dir = TempDir::new("yagura-roles-test", "unknown-key");
    let path = roles_path(&dir);
    write(
        &path,
        "schema = 1\n\n[[role]]\nname = \"impl\"\nprofile = \"codex-cli\"\nbogus = \"field\"\n",
    );

    assert!(
        read_roles(&path).is_err(),
        "an unknown key in a [[role]] table must be rejected (#[serde(deny_unknown_fields)])"
    );
}

#[test]
fn read_roles_rejects_a_duplicate_role_name() {
    let dir = TempDir::new("yagura-roles-test", "duplicate-name");
    let path = roles_path(&dir);
    write(
        &path,
        "schema = 1\n\n\
         [[role]]\nname = \"impl\"\nprofile = \"codex-cli\"\n\n\
         [[role]]\nname = \"impl\"\nprofile = \"claude-code\"\n",
    );

    assert!(
        read_roles(&path).is_err(),
        "two rows sharing the same role name must be rejected"
    );
}

#[test]
fn read_roles_rejects_an_invalid_role_name_format() {
    let dir = TempDir::new("yagura-roles-test", "invalid-name");
    let path = roles_path(&dir);
    // Uppercase is outside is_valid_token's [a-z0-9_-] character set.
    write(
        &path,
        "schema = 1\n\n[[role]]\nname = \"Invalid Name\"\nprofile = \"codex-cli\"\n",
    );

    assert!(
        read_roles(&path).is_err(),
        "a role name failing is_valid_token must be rejected"
    );
}

#[test]
fn read_roles_rejects_an_invalid_model_value() {
    let dir = TempDir::new("yagura-roles-test", "invalid-model");
    let path = roles_path(&dir);
    // A space is outside validate_launch_option_value's argv token set.
    write(
        &path,
        "schema = 1\n\n[[role]]\nname = \"impl\"\nprofile = \"codex-cli\"\nmodel = \"bad value\"\n",
    );

    assert!(
        read_roles(&path).is_err(),
        "a model value failing validate_launch_option_value must be rejected"
    );
}

#[test]
fn read_roles_rejects_an_invalid_effort_value() {
    let dir = TempDir::new("yagura-roles-test", "invalid-effort");
    let path = roles_path(&dir);
    // A leading '-' is rejected by validate_launch_option_value.
    write(
        &path,
        "schema = 1\n\n[[role]]\nname = \"impl\"\nprofile = \"codex-cli\"\neffort = \"-bad\"\n",
    );

    assert!(
        read_roles(&path).is_err(),
        "an effort value starting with '-' must be rejected"
    );
}

#[test]
fn read_roles_accepts_the_row_limit_boundary() {
    let dir = TempDir::new("yagura-roles-test", "row-limit-ok");
    let path = roles_path(&dir);
    write(&path, &toml_with_n_distinct_rows(MAX_ROLE_ROWS));

    let table = read_roles(&path).unwrap_or_else(|err| {
        panic!("exactly MAX_ROLE_ROWS ({MAX_ROLE_ROWS}) rows must be accepted, got {err:?}")
    });
    assert_eq!(table.rows.len(), MAX_ROLE_ROWS);
}

#[test]
fn read_roles_rejects_one_row_over_the_limit() {
    let dir = TempDir::new("yagura-roles-test", "row-limit-over");
    let path = roles_path(&dir);
    write(&path, &toml_with_n_distinct_rows(MAX_ROLE_ROWS + 1));

    assert!(
        read_roles(&path).is_err(),
        "MAX_ROLE_ROWS + 1 rows must be rejected"
    );
}

#[test]
fn read_roles_rejects_the_whole_table_when_only_one_row_among_several_is_invalid() {
    let dir = TempDir::new("yagura-roles-test", "no-rounding");
    let path = roles_path(&dir);
    // Two distinct, valid rows plus a third that repeats the first row's name.
    write(
        &path,
        "schema = 1\n\n\
         [[role]]\nname = \"impl\"\nprofile = \"codex-cli\"\n\n\
         [[role]]\nname = \"review\"\nprofile = \"claude-code\"\n\n\
         [[role]]\nname = \"impl\"\nprofile = \"shell\"\n",
    );

    assert!(
        read_roles(&path).is_err(),
        "a single bad row must take the whole table down rather than being dropped while the \
         two good rows are returned"
    );
}

// ---------------------------------------------------------------------
// save_roles
// ---------------------------------------------------------------------

#[test]
fn save_roles_writes_a_valid_table_and_it_round_trips_through_read_roles() {
    let dir = TempDir::new("yagura-roles-test", "save-round-trip");
    let path = roles_path(&dir);
    let table = RolesTable {
        rows: vec![
            RoleRow {
                name: "impl".to_string(),
                profile: "codex-cli".to_string(),
                model: Some("default".to_string()),
                effort: Some("high".to_string()),
            },
            RoleRow {
                name: "review".to_string(),
                profile: "claude-code".to_string(),
                model: None,
                effort: None,
            },
        ],
        default_role: None,
    };

    let result = save_roles(&path, &table, |_profile| true);
    assert!(
        result.is_ok(),
        "saving a valid table must succeed: {result:?}"
    );

    let on_disk = std::fs::read_to_string(&path).expect("saved roles.toml must exist");
    assert!(
        on_disk.contains("schema = 1"),
        "the saved file must carry schema = 1, got:\n{on_disk}"
    );

    let read_back = read_roles(&path).expect("the saved file must itself be readable");
    assert_eq!(
        read_back, table,
        "reading the saved file back must reproduce exactly the table that was saved"
    );
}

#[test]
fn save_roles_rejects_an_unknown_profile_and_leaves_the_file_untouched() {
    let dir = TempDir::new("yagura-roles-test", "save-unknown-profile");
    let path = roles_path(&dir);
    let original = "schema = 1\n\n[[role]]\nname = \"kept\"\nprofile = \"codex-cli\"\n";
    write(&path, original);

    let table = RolesTable {
        rows: vec![RoleRow {
            name: "impl".to_string(),
            profile: "no-such-profile".to_string(),
            model: None,
            effort: None,
        }],
        default_role: None,
    };

    let result = save_roles(&path, &table, |_profile| false);
    assert!(
        result.is_err(),
        "saving a row whose profile does not exist must be rejected: {result:?}"
    );

    let on_disk = std::fs::read_to_string(&path).expect("original file must still exist");
    assert_eq!(
        on_disk, original,
        "a rejected save must not touch the file on disk at all"
    );
}

#[test]
fn save_roles_rejects_the_same_shape_violations_as_read_roles_and_leaves_the_file_untouched() {
    let dir = TempDir::new("yagura-roles-test", "save-invalid-shape");
    let path = roles_path(&dir);
    let original = "schema = 1\n\n[[role]]\nname = \"kept\"\nprofile = \"codex-cli\"\n";
    write(&path, original);

    let table = RolesTable {
        rows: vec![RoleRow {
            name: "Invalid Name".to_string(),
            profile: "codex-cli".to_string(),
            model: None,
            effort: None,
        }],
        default_role: None,
    };

    let result = save_roles(&path, &table, |_profile| true);
    assert!(
        result.is_err(),
        "an invalid role name must be rejected on save, the same rule read_roles enforces: \
         {result:?}"
    );

    let on_disk = std::fs::read_to_string(&path).expect("original file must still exist");
    assert_eq!(
        on_disk, original,
        "a rejected save must not touch the file on disk at all"
    );
}

#[test]
fn save_roles_rejects_one_row_over_the_limit() {
    let dir = TempDir::new("yagura-roles-test", "save-row-limit");
    let path = roles_path(&dir);
    let table = RolesTable {
        rows: rows_with_n_distinct_entries(MAX_ROLE_ROWS + 1),
        default_role: None,
    };

    let result = save_roles(&path, &table, |_profile| true);
    assert!(
        result.is_err(),
        "MAX_ROLE_ROWS + 1 rows must be rejected on save, the same limit read_roles enforces: \
         {result:?}"
    );
}

// ---------------------------------------------------------------------
// RolesTable::assignment_for maps a role name into an
// yagura_daemon::plan::Assignment triple, ready for plan::resolve_assignment.
// ---------------------------------------------------------------------

#[test]
fn assignment_for_returns_the_matching_rows_assignment() {
    let table = RolesTable {
        rows: vec![RoleRow {
            name: "impl".to_string(),
            profile: "codex-cli".to_string(),
            model: Some("default".to_string()),
            effort: Some("high".to_string()),
        }],
        default_role: None,
    };

    let resolved = table.assignment_for("impl");
    assert_eq!(
        resolved,
        Some(Assignment {
            profile: "codex-cli".to_string(),
            model: Some("default".to_string()),
            effort: Some("high".to_string()),
        })
    );
}

#[test]
fn assignment_for_returns_none_for_an_absent_role_name() {
    let table = RolesTable {
        rows: vec![RoleRow {
            name: "impl".to_string(),
            profile: "codex-cli".to_string(),
            model: None,
            effort: None,
        }],
        default_role: None,
    };

    assert_eq!(table.assignment_for("no-such-role"), None);
}

#[test]
fn assignment_for_handles_a_row_without_model_or_effort() {
    let table = RolesTable {
        rows: vec![RoleRow {
            name: "review".to_string(),
            profile: "claude-code".to_string(),
            model: None,
            effort: None,
        }],
        default_role: None,
    };

    let resolved = table.assignment_for("review");
    assert_eq!(
        resolved,
        Some(Assignment {
            profile: "claude-code".to_string(),
            model: None,
            effort: None,
        })
    );
}

// ---------------------------------------------------------------------
// the size of the file read
// ---------------------------------------------------------------------

/// The most bytes a `roles.toml` may hold, the same as `tickets.toml`.
const ROLES_MAX_BYTES: usize = 64 * 1024;

/// [`valid_toml`] padded with a trailing comment to exactly `total` bytes.
fn padded_roles(total: usize) -> String {
    let mut text = valid_toml().to_string();
    let padding = total - text.len() - 2;
    text.push('#');
    text.push_str(&"x".repeat(padding));
    text.push('\n');
    assert_eq!(text.len(), total);
    text
}

#[test]
fn read_roles_accepts_a_file_of_exactly_sixty_four_kib_and_rejects_one_byte_more() {
    let dir = TempDir::new("yagura-roles-test", "size");
    let path = roles_path(&dir);

    write(&path, &padded_roles(ROLES_MAX_BYTES));
    let at_limit = read_roles(&path);
    assert!(
        matches!(&at_limit, Ok(table) if table.rows.len() == 2),
        "a file of exactly 64 KiB is within the limit, got {at_limit:?}"
    );

    write(&path, &padded_roles(ROLES_MAX_BYTES + 1));
    let over = read_roles(&path);
    assert!(
        over.is_err(),
        "a file over 64 KiB must be refused, got {over:?}"
    );
}
