#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `roles::read_roles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! The role table's `default_role`: the row a task falls back to when it
//! names no role or names one the table has no row for.
//!
//! A `default_role` that does not name one of the table's own rows makes the
//! whole table invalid, on read and on save alike, because it is an internal
//! inconsistency of the document rather than an external drift.

use std::path::{Path, PathBuf};

use yagura_daemon::plan::{Assignment, AssignmentSource, resolve_assignment};
use yagura_daemon::roles::{RoleRow, RolesError, RolesTable, read_roles, save_roles};
use yagura_test_support::paths::TempDir;

fn roles_path(dir: &TempDir) -> PathBuf {
    dir.path().join("roles.toml")
}

fn write(path: &Path, content: &str) {
    std::fs::write(path, content).expect("fixture roles.toml should be writable");
}

const TWO_ROWS: &str = "\n[[role]]\nname = \"impl\"\nprofile = \"codex-cli\"\nmodel = \"fast\"\n\n\
                        [[role]]\nname = \"review\"\nprofile = \"claude-code\"\n";

fn toml_with_default(default_role: &str) -> String {
    format!("schema = 1\ndefault_role = \"{default_role}\"\n{TWO_ROWS}")
}

fn toml_without_default() -> String {
    format!("schema = 1\n{TWO_ROWS}")
}

fn row(name: &str, profile: &str, model: Option<&str>) -> RoleRow {
    RoleRow {
        name: name.to_string(),
        profile: profile.to_string(),
        model: model.map(str::to_string),
        effort: None,
    }
}

fn table(default_role: Option<&str>) -> RolesTable {
    RolesTable {
        rows: vec![
            row("impl", "codex-cli", Some("fast")),
            row("review", "claude-code", None),
        ],
        default_role: default_role.map(str::to_string),
    }
}

fn impl_assignment() -> Assignment {
    Assignment {
        profile: "codex-cli".to_string(),
        model: Some("fast".to_string()),
        effort: None,
    }
}

fn review_assignment() -> Assignment {
    Assignment {
        profile: "claude-code".to_string(),
        model: None,
        effort: None,
    }
}

fn any_profile_exists(_: &str) -> bool {
    true
}

// --- read ----------------------------------------------------------------

#[test]
fn read_roles_keeps_a_default_role_naming_one_of_its_rows() {
    let dir = TempDir::new("yagura-roles-default", "read-ok");
    let path = roles_path(&dir);
    write(&path, &toml_with_default("impl"));

    let read = read_roles(&path).expect("a default naming an existing row must load");
    assert_eq!(read.default_role.as_deref(), Some("impl"));
    assert_eq!(read.rows.len(), 2);
}

#[test]
fn read_roles_without_a_default_role_key_reads_as_none() {
    let dir = TempDir::new("yagura-roles-default", "read-absent");
    let path = roles_path(&dir);
    write(&path, &toml_without_default());

    let read = read_roles(&path).expect("a table without default_role must still load");
    assert_eq!(read.default_role, None);
    assert_eq!(read.rows.len(), 2);
}

#[test]
fn read_roles_rejects_a_default_role_the_table_has_no_row_for() {
    let dir = TempDir::new("yagura-roles-default", "read-unknown");
    let path = roles_path(&dir);
    write(&path, &toml_with_default("docs"));

    let err = read_roles(&path).expect_err("a default naming no row must reject the table");
    assert!(
        matches!(err, RolesError::UnknownDefaultRole),
        "expected UnknownDefaultRole, got {err:?}"
    );
}

#[test]
fn read_roles_rejects_a_default_role_that_is_not_a_token() {
    let dir = TempDir::new("yagura-roles-default", "read-not-token");
    let path = roles_path(&dir);
    write(&path, &toml_with_default("not a token!"));

    let err = read_roles(&path).expect_err("a default that is not a token must reject the table");
    assert!(
        matches!(err, RolesError::UnknownDefaultRole),
        "expected UnknownDefaultRole, got {err:?}"
    );
}

// --- save ----------------------------------------------------------------

#[test]
fn save_roles_writes_the_default_role_and_it_round_trips_through_read_roles() {
    let dir = TempDir::new("yagura-roles-default", "save-round-trip");
    let path = roles_path(&dir);

    save_roles(&path, &table(Some("review")), any_profile_exists)
        .expect("a default naming an existing row must save");
    let written = std::fs::read_to_string(&path).expect("the saved file must be readable");
    assert!(
        written.contains("default_role"),
        "the saved document must carry the default_role key: {written}"
    );
    assert_eq!(
        read_roles(&path).expect("the saved table must read back"),
        table(Some("review"))
    );
}

#[test]
fn save_roles_without_a_default_role_writes_no_default_role_key() {
    let dir = TempDir::new("yagura-roles-default", "save-none");
    let path = roles_path(&dir);

    save_roles(&path, &table(None), any_profile_exists).expect("a table without a default saves");
    let written = std::fs::read_to_string(&path).expect("the saved file must be readable");
    assert!(
        !written.contains("default_role"),
        "an absent default must not be written, so an older build still reads the file: {written}"
    );
    assert_eq!(
        read_roles(&path).expect("the saved table must read back"),
        table(None)
    );
}

#[test]
fn save_roles_rejects_an_unknown_or_malformed_default_role_and_leaves_the_file_untouched() {
    let dir = TempDir::new("yagura-roles-default", "save-rejected");
    let path = roles_path(&dir);
    let original = toml_with_default("impl");
    write(&path, &original);

    for bad in ["docs", "not a token!"] {
        let err = save_roles(&path, &table(Some(bad)), any_profile_exists)
            .expect_err("a default naming no row must be refused");
        assert!(
            matches!(err, RolesError::UnknownDefaultRole),
            "expected UnknownDefaultRole for {bad:?}, got {err:?}"
        );
        assert!(
            !err.to_string().contains(bad),
            "the refusal must not echo the rejected value: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("the file must still be readable"),
            original,
            "a refused save must not touch the file"
        );
    }
}

// --- role and default lookup ---------------------------------------------

#[test]
fn role_and_default_for_returns_only_the_role_row_when_the_role_is_in_the_table() {
    let (role, default) = table(Some("impl")).role_and_default_for(Some("review"));
    assert_eq!(role, Some(review_assignment()));
    assert_eq!(default, None);
}

#[test]
fn role_and_default_for_returns_the_default_row_when_the_task_names_no_role() {
    let (role, default) = table(Some("impl")).role_and_default_for(None);
    assert_eq!(role, None);
    assert_eq!(default, Some(impl_assignment()));
}

#[test]
fn role_and_default_for_returns_the_default_row_when_the_role_is_not_in_the_table() {
    let (role, default) = table(Some("impl")).role_and_default_for(Some("docs"));
    assert_eq!(role, None);
    assert_eq!(default, Some(impl_assignment()));
}

#[test]
fn role_and_default_for_returns_nothing_without_a_default_role() {
    assert_eq!(table(None).role_and_default_for(None), (None, None));
    assert_eq!(table(None).role_and_default_for(Some("docs")), (None, None));
}

// --- resolution with a default -------------------------------------------

fn resolve_for_role(
    table: &RolesTable,
    role: Option<&str>,
) -> (AssignmentSource, Option<Assignment>) {
    let (role, default) = table.role_and_default_for(role);
    let resolved = resolve_assignment(None, None, role.as_ref(), default.as_ref());
    (resolved.source, resolved.assignment)
}

#[test]
fn a_task_without_a_role_resolves_to_the_default_row() {
    assert_eq!(
        resolve_for_role(&table(Some("impl")), None),
        (AssignmentSource::Default, Some(impl_assignment()))
    );
}

#[test]
fn a_task_whose_role_the_table_lacks_resolves_to_the_default_row() {
    assert_eq!(
        resolve_for_role(&table(Some("impl")), Some("docs")),
        (AssignmentSource::Default, Some(impl_assignment()))
    );
}

#[test]
fn a_task_whose_role_the_table_has_resolves_to_that_row_not_the_default() {
    assert_eq!(
        resolve_for_role(&table(Some("impl")), Some("review")),
        (AssignmentSource::Role, Some(review_assignment()))
    );
}

#[test]
fn a_user_override_and_a_parent_assignment_each_win_over_the_default() {
    let user = Assignment {
        profile: "user-profile".to_string(),
        model: None,
        effort: None,
    };
    let parent = Assignment {
        profile: "parent-profile".to_string(),
        model: None,
        effort: None,
    };
    let default = impl_assignment();

    let resolved = resolve_assignment(Some(&user), None, None, Some(&default));
    assert_eq!(resolved.source, AssignmentSource::User);
    assert_eq!(resolved.assignment, Some(user));

    let resolved = resolve_assignment(None, Some(&parent), None, Some(&default));
    assert_eq!(resolved.source, AssignmentSource::Parent);
    assert_eq!(resolved.assignment, Some(parent));
}

#[test]
fn an_unreadable_table_resolves_a_task_to_unassigned() {
    let dir = TempDir::new("yagura-roles-default", "unreadable");
    let path = roles_path(&dir);
    write(&path, &toml_with_default("docs"));

    let (role, default) = match read_roles(&path) {
        Ok(table) => table.role_and_default_for(None),
        Err(_) => (None, None),
    };
    let resolved = resolve_assignment(None, None, role.as_ref(), default.as_ref());
    assert_eq!(resolved.source, AssignmentSource::Unassigned);
    assert_eq!(resolved.assignment, None);
}
