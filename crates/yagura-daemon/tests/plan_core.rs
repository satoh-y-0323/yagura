#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the plan-core pure-function module (`yagura_daemon::plan`) --
//! validation, write-target normalization, wave derivation and assignment resolution.
//!
//! `plan` is a pure-function module: it owns no I/O, no environment, no registry and no
//! store. Profile existence is handed in as a predicate rather than looked up, and a role
//! name that is not in any table is not this module's concern -- the caller resolves role
//! names against `roles.toml` before calling [`resolve_assignment`].

use yagura_daemon::plan::{
    Assignment, AssignmentSource, PlanError, PlanSpec, TaskSpec, WriteConflict, WritePathError,
    compute_waves, normalize_write_path, resolve_assignment, validate_plan,
};

const MAX_PLAN_TASKS: usize = 256;
const MAX_TASK_WRITES: usize = 64;
const MAX_WRITE_PATH_BYTES: usize = 512;

// --- builders ---------------------------------------------------------------

fn task(task_id: &str) -> TaskSpec {
    TaskSpec {
        task_id: task_id.to_string(),
        title: format!("Task {task_id}"),
        role: None,
        profile: None,
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

fn depends_on(mut t: TaskSpec, deps: &[&str]) -> TaskSpec {
    t.depends_on = deps.iter().map(|s| s.to_string()).collect();
    t
}

fn writes(mut t: TaskSpec, paths: &[&str]) -> TaskSpec {
    t.writes = paths.iter().map(|s| s.to_string()).collect();
    t
}

fn plan_of(tasks: Vec<TaskSpec>) -> PlanSpec {
    PlanSpec {
        title: "Plan".to_string(),
        tasks,
    }
}

fn no_profiles(_id: &str) -> bool {
    false
}

fn only_known_profile(id: &str) -> bool {
    id == "known-profile"
}

fn task_ids(errors: &[PlanError]) -> Vec<String> {
    errors.iter().map(|e| e.to_string()).collect()
}

// --- wave derivation (`compute_waves`) --------------------------------

#[test]
fn compute_waves_returns_the_position_along_a_linear_chain() {
    let tasks = vec![
        task("a"),
        depends_on(task("b"), &["a"]),
        depends_on(task("c"), &["b"]),
    ];
    assert_eq!(compute_waves(&tasks), vec![0, 1, 2]);
}

#[test]
fn compute_waves_returns_the_longest_chain_length_for_a_diamond() {
    // a -> {b, c} -> d
    let tasks = vec![
        task("a"),
        depends_on(task("b"), &["a"]),
        depends_on(task("c"), &["a"]),
        depends_on(task("d"), &["b", "c"]),
    ];
    assert_eq!(compute_waves(&tasks), vec![0, 1, 1, 2]);
}

#[test]
fn compute_waves_treats_dependency_free_tasks_as_wave_zero() {
    let tasks = vec![task("a"), task("b"), task("c")];
    assert_eq!(compute_waves(&tasks), vec![0, 0, 0]);
}

// --- wave grouping order and determinism via validate_plan -------------------

#[test]
fn pc_w1_groups_wave_members_in_registration_order() {
    // a -> {b, c} -> d, registered a, b, c, d.
    let spec = plan_of(vec![
        task("a"),
        depends_on(task("b"), &["a"]),
        depends_on(task("c"), &["a"]),
        depends_on(task("d"), &["b", "c"]),
    ]);
    let validated = validate_plan(&spec, no_profiles).expect("acyclic plan validates");
    assert_eq!(
        validated.waves,
        vec![
            vec!["a".to_string()],
            vec!["b".to_string(), "c".to_string()],
            vec!["d".to_string()],
        ]
    );
}

#[test]
fn pc_w2_wave_numbers_are_stable_when_tasks_are_reordered_but_grouping_follows_the_new_order() {
    let ordered_a_b_c_d = plan_of(vec![
        task("a"),
        depends_on(task("b"), &["a"]),
        depends_on(task("c"), &["a"]),
        depends_on(task("d"), &["b", "c"]),
    ]);
    let ordered_a_c_b_d = plan_of(vec![
        task("a"),
        depends_on(task("c"), &["a"]),
        depends_on(task("b"), &["a"]),
        depends_on(task("d"), &["b", "c"]),
    ]);

    let first = validate_plan(&ordered_a_b_c_d, no_profiles).expect("acyclic plan validates");
    let second = validate_plan(&ordered_a_c_b_d, no_profiles).expect("acyclic plan validates");

    let wave_of = |plan: &yagura_daemon::plan::ValidatedPlan, id: &str| {
        plan.tasks
            .iter()
            .find(|t| t.task_id == id)
            .expect("task present")
            .wave
    };
    for id in ["a", "b", "c", "d"] {
        assert_eq!(
            wave_of(&first, id),
            wave_of(&second, id),
            "wave number for {id} must not depend on registration order"
        );
    }

    // Only the *grouping order* within a wave follows the new registration order.
    assert_eq!(
        first.waves,
        vec![
            vec!["a".to_string()],
            vec!["b".to_string(), "c".to_string()],
            vec!["d".to_string()],
        ]
    );
    assert_eq!(
        second.waves,
        vec![
            vec!["a".to_string()],
            vec!["c".to_string(), "b".to_string()],
            vec!["d".to_string()],
        ]
    );
}

// --- plan and task shape ---------------------------------------------

#[test]
fn pc_v1_rejects_a_plan_with_zero_tasks() {
    let spec = plan_of(vec![]);
    let errors = validate_plan(&spec, no_profiles).expect_err("empty plan is rejected");
    assert!(errors.iter().any(|e| matches!(e, PlanError::NoTasks)));
}

#[test]
fn pc_v1_rejects_a_plan_over_the_task_count_limit() {
    let tasks: Vec<TaskSpec> = (0..=MAX_PLAN_TASKS)
        .map(|i| writes(task(&format!("t{i}")), &[&format!("t{i}.txt")]))
        .collect();
    let spec = plan_of(tasks);
    let errors = validate_plan(&spec, no_profiles).expect_err("over-limit plan is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::TooManyTasks { max, actual }
                if *max == MAX_PLAN_TASKS && *actual == MAX_PLAN_TASKS + 1))
    );
}

#[test]
fn pc_v1_rejects_a_duplicate_task_id() {
    let spec = plan_of(vec![task("dup"), task("dup")]);
    let errors = validate_plan(&spec, no_profiles).expect_err("duplicate task_id is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::DuplicateTaskId))
    );
}

#[test]
fn pc_v1_rejects_a_task_id_outside_the_token_charset() {
    let spec = plan_of(vec![task("Bad Task!")]);
    let errors = validate_plan(&spec, no_profiles).expect_err("malformed task_id is rejected");
    assert!(errors.iter().any(|e| matches!(e, PlanError::InvalidTaskId)));
}

#[test]
fn pc_v1_rejects_a_plan_title_over_the_window_name_byte_limit() {
    let mut spec = plan_of(vec![task("a")]);
    spec.title = "x".repeat(129);
    let errors = validate_plan(&spec, no_profiles).expect_err("over-long plan title is rejected");
    assert!(errors.iter().any(|e| matches!(e, PlanError::InvalidTitle)));
}

#[test]
fn pc_v1_rejects_a_task_title_over_the_window_name_byte_limit() {
    let mut t = task("a");
    t.title = "x".repeat(129);
    let spec = plan_of(vec![t]);
    let errors = validate_plan(&spec, no_profiles).expect_err("over-long task title is rejected");
    assert!(errors.iter().any(|e| matches!(e, PlanError::InvalidTitle)));
}

#[test]
fn pc_v1_rejects_a_role_name_outside_the_token_charset() {
    let mut t = task("a");
    t.role = Some("Bad-Role!".to_string());
    let spec = plan_of(vec![t]);
    let errors = validate_plan(&spec, no_profiles).expect_err("malformed role name is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::InvalidRoleName))
    );
}

// --- dependencies ----------------------------------------------------

#[test]
fn pc_v2_rejects_a_dependency_on_a_task_id_that_does_not_exist() {
    let spec = plan_of(vec![depends_on(task("a"), &["ghost"])]);
    let errors = validate_plan(&spec, no_profiles).expect_err("dangling dependency is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::UnknownDependency))
    );
}

#[test]
fn pc_v2_rejects_a_task_that_depends_on_itself() {
    let spec = plan_of(vec![depends_on(task("a"), &["a"])]);
    let errors = validate_plan(&spec, no_profiles).expect_err("self dependency is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::SelfDependency))
    );
}

#[test]
fn pc_v2_rejects_a_repeated_dependency_entry_within_one_task() {
    let spec = plan_of(vec![task("a"), depends_on(task("b"), &["a", "a"])]);
    let errors =
        validate_plan(&spec, no_profiles).expect_err("duplicated depends_on entry is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::DuplicateDependency))
    );
}

#[test]
fn pc_v2_rejects_a_cycle_and_names_every_task_in_it() {
    let spec = plan_of(vec![
        depends_on(task("a"), &["b"]),
        depends_on(task("b"), &["a"]),
    ]);
    let errors = validate_plan(&spec, no_profiles).expect_err("a cycle is rejected");
    let cycle = errors
        .iter()
        .find_map(|e| match e {
            PlanError::Cycle { task_ids } => Some(task_ids.clone()),
            _ => None,
        })
        .expect("a Cycle error is reported");
    let mut sorted = cycle;
    sorted.sort();
    assert_eq!(sorted, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn pc_v2_a_cycle_prevents_wave_and_conflict_validation_from_running() {
    // a <-> b form a cycle; c and d are unrelated and write the same path without
    // being ordered by any dependency, which would normally be a write conflict.
    let spec = plan_of(vec![
        depends_on(task("a"), &["b"]),
        depends_on(task("b"), &["a"]),
        writes(task("c"), &["same.txt"]),
        writes(task("d"), &["same.txt"]),
    ]);
    let errors = validate_plan(&spec, no_profiles).expect_err("a cycle is rejected");
    assert!(errors.iter().any(|e| matches!(e, PlanError::Cycle { .. })));
    assert!(
        !errors
            .iter()
            .any(|e| matches!(e, PlanError::WriteConflict { .. })),
        "conflict validation must not run once a cycle is found: {:?}",
        task_ids(&errors)
    );
}

#[test]
fn pc_v2_accepts_the_maximum_fan_in_at_the_task_count_limit() {
    // MAX_PLAN_TASKS tasks; the last one depends on every one of the others.
    let mut tasks: Vec<TaskSpec> = (0..MAX_PLAN_TASKS - 1)
        .map(|i| writes(task(&format!("t{i}")), &[&format!("t{i}.txt")]))
        .collect();
    let preceding: Vec<&str> = tasks.iter().map(|t| t.task_id.as_str()).collect();
    let last = depends_on(writes(task("last"), &["last.txt"]), preceding.as_slice());
    tasks.push(last);
    assert_eq!(tasks.len(), MAX_PLAN_TASKS);

    let spec = plan_of(tasks);
    let validated =
        validate_plan(&spec, no_profiles).expect("maximum fan-in at the task limit is accepted");
    assert_eq!(validated.tasks.len(), MAX_PLAN_TASKS);
}

// --- parent specification --------------------------------------------

#[test]
fn pc_v3_rejects_model_specified_without_a_profile() {
    let mut t = task("a");
    t.model = Some("fast".to_string());
    let spec = plan_of(vec![t]);
    let errors = validate_plan(&spec, no_profiles).expect_err("model without profile is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::LaunchOptionWithoutProfile))
    );
}

#[test]
fn pc_v3_rejects_effort_specified_without_a_profile() {
    let mut t = task("a");
    t.effort = Some("high".to_string());
    let spec = plan_of(vec![t]);
    let errors = validate_plan(&spec, no_profiles).expect_err("effort without profile is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::LaunchOptionWithoutProfile))
    );
}

#[test]
fn pc_v3_rejects_a_profile_id_that_does_not_exist() {
    let mut t = task("a");
    t.profile = Some("ghost".to_string());
    let spec = plan_of(vec![t]);
    let errors = validate_plan(&spec, only_known_profile).expect_err("unknown profile is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::UnknownProfile))
    );
}

#[test]
fn pc_v3_rejects_a_model_value_outside_the_launch_option_rule() {
    let mut t = task("a");
    t.profile = Some("known-profile".to_string());
    t.model = Some("bad;value".to_string());
    let spec = plan_of(vec![t]);
    let errors =
        validate_plan(&spec, only_known_profile).expect_err("malformed model value is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::InvalidLaunchOptionValue))
    );
}

#[test]
fn pc_v3_rejects_an_effort_value_outside_the_launch_option_rule() {
    let mut t = task("a");
    t.profile = Some("known-profile".to_string());
    t.effort = Some("-leading-dash".to_string());
    let spec = plan_of(vec![t]);
    let errors =
        validate_plan(&spec, only_known_profile).expect_err("malformed effort value is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::InvalidLaunchOptionValue))
    );
}

// --- write-target path normalization (direct) ------------------------

#[test]
fn normalize_write_path_converts_backslashes_to_forward_slashes() {
    assert_eq!(
        normalize_write_path("src\\lib.rs"),
        Ok("src/lib.rs".to_string())
    );
}

#[test]
fn normalize_write_path_collapses_leading_dot_slash_and_repeated_slashes() {
    assert_eq!(
        normalize_write_path("./src//lib.rs"),
        Ok("src/lib.rs".to_string())
    );
}

#[test]
fn normalize_write_path_rejects_an_absolute_path() {
    assert_eq!(
        normalize_write_path("/etc/passwd"),
        Err(WritePathError::Absolute)
    );
}

#[test]
fn normalize_write_path_rejects_a_drive_letter() {
    assert_eq!(
        normalize_write_path("C:\\Windows\\System32"),
        Err(WritePathError::DriveLetter)
    );
}

#[test]
fn normalize_write_path_rejects_parent_traversal() {
    assert_eq!(
        normalize_write_path("../outside.rs"),
        Err(WritePathError::ParentTraversal)
    );
}

#[test]
fn normalize_write_path_rejects_an_empty_path() {
    assert_eq!(normalize_write_path(""), Err(WritePathError::Empty));
}

#[test]
fn normalize_write_path_rejects_a_path_over_the_byte_limit() {
    let over_limit = "a".repeat(MAX_WRITE_PATH_BYTES + 1);
    assert_eq!(
        normalize_write_path(&over_limit),
        Err(WritePathError::TooLong {
            max: MAX_WRITE_PATH_BYTES,
            actual: MAX_WRITE_PATH_BYTES + 1,
        })
    );
}

#[test]
fn normalize_write_path_keeps_a_space_inside_a_file_name() {
    // A space is the first byte past the control range, so it sits on the boundary.
    assert_eq!(
        normalize_write_path("docs/my notes.md"),
        Ok("docs/my notes.md".to_string())
    );
}

#[test]
fn normalize_write_path_rejects_a_control_character() {
    assert_eq!(
        normalize_write_path("src/lib\u{0000}.rs"),
        Err(WritePathError::ForbiddenChar)
    );
}

#[test]
fn normalize_write_path_rejects_a_double_quote() {
    assert_eq!(
        normalize_write_path("src/\"lib.rs"),
        Err(WritePathError::ForbiddenChar)
    );
}

#[test]
fn normalize_write_path_preserves_the_original_case() {
    assert_eq!(
        normalize_write_path("Src\\LiB.rs"),
        Ok("Src/LiB.rs".to_string())
    );
}

// --- per-task write count and intra-task duplicates ------------------

#[test]
fn pc_v4_rejects_a_task_with_65_write_targets() {
    let over_limit: Vec<String> = (0..=MAX_TASK_WRITES).map(|i| format!("w{i}.txt")).collect();
    let over_limit_refs: Vec<&str> = over_limit.iter().map(String::as_str).collect();
    let spec = plan_of(vec![writes(task("a"), &over_limit_refs)]);
    let errors = validate_plan(&spec, no_profiles).expect_err("over-limit writes are rejected");
    assert!(errors.iter().any(|e| matches!(e, PlanError::TooManyWrites)));
}

#[test]
fn pc_v4_rejects_a_case_insensitive_duplicate_write_within_one_task() {
    let spec = plan_of(vec![writes(task("a"), &["Dir/File.txt", "dir/file.txt"])]);
    let errors = validate_plan(&spec, no_profiles)
        .expect_err("case-insensitive duplicate write is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::DuplicateWrite))
    );
}

// --- write conflicts across tasks -------------------------------------

#[test]
fn pc_v5_rejects_the_same_path_from_two_unordered_tasks() {
    let spec = plan_of(vec![
        writes(task("a"), &["shared.txt"]),
        writes(task("b"), &["shared.txt"]),
    ]);
    let errors =
        validate_plan(&spec, no_profiles).expect_err("unordered write conflict is rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::WriteConflict { .. }))
    );
}

#[test]
fn pc_v5_allows_the_same_path_from_two_transitively_ordered_tasks() {
    // a -> b -> c, a and c both write "shared.txt": ordered transitively via b.
    let spec = plan_of(vec![
        writes(task("a"), &["shared.txt"]),
        depends_on(task("b"), &["a"]),
        depends_on(writes(task("c"), &["shared.txt"]), &["b"]),
    ]);
    let validated = validate_plan(&spec, no_profiles)
        .expect("a transitively ordered pair sharing a write target is accepted");
    assert_eq!(validated.tasks.len(), 3);
}

#[test]
fn pc_v5_treats_case_only_differences_as_the_same_path_for_conflicts() {
    let spec = plan_of(vec![
        writes(task("a"), &["Shared/Path.txt"]),
        writes(task("b"), &["shared/path.txt"]),
    ]);
    let errors =
        validate_plan(&spec, no_profiles).expect_err("case-only differing paths still conflict");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, PlanError::WriteConflict { .. }))
    );
}

#[test]
fn pc_v5_reports_every_conflicting_pair_in_one_response() {
    let spec = plan_of(vec![
        writes(task("a"), &["shared.txt"]),
        writes(task("b"), &["shared.txt"]),
        writes(task("c"), &["shared.txt"]),
    ]);
    let errors = validate_plan(&spec, no_profiles).expect_err("write conflicts are rejected");
    let conflicts: Vec<WriteConflict> = errors
        .into_iter()
        .find_map(|e| match e {
            PlanError::WriteConflict { conflicts } => Some(conflicts),
            _ => None,
        })
        .expect("a WriteConflict error is reported");
    let mut pairs: Vec<(String, String)> = conflicts
        .iter()
        .map(|c| {
            let mut pair = [c.task_a.clone(), c.task_b.clone()];
            pair.sort();
            (pair[0].clone(), pair[1].clone())
        })
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("a".to_string(), "b".to_string()),
            ("a".to_string(), "c".to_string()),
            ("b".to_string(), "c".to_string()),
        ]
    );
}

// --- rejected values are never echoed ---------------------------------

#[test]
fn pc_v6_rejected_title_error_does_not_display_the_title_text() {
    let mut spec = plan_of(vec![task("a")]);
    // A forbidden control character forces rejection; the marker string must not
    // leak into any error's Display output.
    spec.title = "SECRET_TITLE_MARKER\u{0000}".to_string();
    let errors =
        validate_plan(&spec, no_profiles).expect_err("control character title is rejected");
    for e in &errors {
        assert!(
            !e.to_string().contains("SECRET_TITLE_MARKER"),
            "error must not echo the rejected title: {e}"
        );
    }
}

#[test]
fn pc_v6_rejected_write_path_error_does_not_display_the_path_text() {
    let err = normalize_write_path("SECRET_PATH_MARKER\u{0000}.txt")
        .expect_err("control character path is rejected");
    assert!(
        !err.to_string().contains("SECRET_PATH_MARKER"),
        "error must not echo the rejected path: {err}"
    );
}

#[test]
fn pc_v6_conflict_report_may_name_the_validated_task_ids_and_normalized_path() {
    let spec = plan_of(vec![
        writes(task("owner-a"), &["Shared/Path.txt"]),
        writes(task("owner-b"), &["shared/path.txt"]),
    ]);
    let errors = validate_plan(&spec, no_profiles).expect_err("write conflict is rejected");
    let conflicts = errors
        .into_iter()
        .find_map(|e| match e {
            PlanError::WriteConflict { conflicts } => Some(conflicts),
            _ => None,
        })
        .expect("a WriteConflict error is reported");
    assert_eq!(conflicts.len(), 1);
    let mut ids = vec![conflicts[0].task_a.clone(), conflicts[0].task_b.clone()];
    ids.sort();
    assert_eq!(ids, vec!["owner-a".to_string(), "owner-b".to_string()]);
    assert_eq!(conflicts[0].path.to_lowercase(), "shared/path.txt");
}

// --- an unregistered role name is not a validation error --------------

#[test]
fn pc_v7_an_unregistered_role_name_is_not_a_validation_error() {
    let mut t = task("a");
    t.role = Some("no-such-role-anywhere".to_string());
    let spec = plan_of(vec![t]);
    // `plan` never receives a role table, so it cannot and must not reject a
    // well-formed role name for being absent from one.
    let validated = validate_plan(&spec, no_profiles)
        .expect("a well-formed but unregistered role name validates");
    assert_eq!(
        validated.tasks[0].role.as_deref(),
        Some("no-such-role-anywhere")
    );
}

// --- assignment resolution ----------------------------------------------

fn sample_user() -> Assignment {
    Assignment {
        profile: "user-profile".to_string(),
        model: Some("user-model".to_string()),
        effort: None,
    }
}

fn sample_parent() -> Assignment {
    Assignment {
        profile: "parent-profile".to_string(),
        model: Some("parent-model".to_string()),
        effort: Some("parent-effort".to_string()),
    }
}

fn sample_role() -> Assignment {
    Assignment {
        profile: "role-profile".to_string(),
        model: None,
        effort: Some("role-effort".to_string()),
    }
}

#[test]
fn pc_r1_user_override_wins_over_parent_and_role() {
    let resolved = resolve_assignment(
        Some(&sample_user()),
        Some(&sample_parent()),
        Some(&sample_role()),
        None,
    );
    assert_eq!(resolved.source, AssignmentSource::User);
    let assignment = resolved.assignment.expect("an assignment is resolved");
    assert_eq!(assignment.profile, "user-profile");
    assert_eq!(assignment.model.as_deref(), Some("user-model"));
    assert_eq!(assignment.effort, None);
}

#[test]
fn pc_r1_parent_wins_over_role_when_no_user_override() {
    let resolved = resolve_assignment(None, Some(&sample_parent()), Some(&sample_role()), None);
    assert_eq!(resolved.source, AssignmentSource::Parent);
    let assignment = resolved.assignment.expect("an assignment is resolved");
    assert_eq!(assignment.profile, "parent-profile");
    assert_eq!(assignment.model.as_deref(), Some("parent-model"));
    assert_eq!(assignment.effort.as_deref(), Some("parent-effort"));
}

#[test]
fn pc_r1_role_wins_when_no_user_or_parent() {
    let resolved = resolve_assignment(None, None, Some(&sample_role()), None);
    assert_eq!(resolved.source, AssignmentSource::Role);
    let assignment = resolved.assignment.expect("an assignment is resolved");
    assert_eq!(assignment.profile, "role-profile");
    assert_eq!(assignment.model, None);
    assert_eq!(assignment.effort.as_deref(), Some("role-effort"));
}

#[test]
fn pc_r1_is_unassigned_when_no_source_is_present() {
    let resolved = resolve_assignment(None, None, None, None);
    assert_eq!(resolved.source, AssignmentSource::Unassigned);
    assert_eq!(resolved.assignment, None);
}

#[test]
fn pc_r1_does_not_mix_fields_from_different_sources() {
    // The user override supplies only a profile; model/effort stay None rather than
    // falling back to the parent's values field-by-field.
    let user_profile_only = Assignment {
        profile: "user-profile".to_string(),
        model: None,
        effort: None,
    };
    let resolved = resolve_assignment(
        Some(&user_profile_only),
        Some(&sample_parent()),
        Some(&sample_role()),
        None,
    );
    assert_eq!(resolved.source, AssignmentSource::User);
    let assignment = resolved.assignment.expect("an assignment is resolved");
    assert_eq!(assignment.profile, "user-profile");
    assert_eq!(
        assignment.model, None,
        "model must not fall back to the parent's value"
    );
    assert_eq!(
        assignment.effort, None,
        "effort must not fall back to the role's value"
    );
}

#[test]
fn pc_r2_a_role_source_that_the_table_could_not_supply_resolves_to_unassigned() {
    // Caller passes role=None when the table lookup found nothing or the table
    // itself was unusable; the resolved outcome must fall through, not error out.
    let resolved = resolve_assignment(None, None, None, None);
    assert_eq!(resolved.source, AssignmentSource::Unassigned);
    assert_eq!(resolved.assignment, None);
}
