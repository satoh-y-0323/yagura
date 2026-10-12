//! Plan-core: pure-function validation, write-target normalization, wave
//! derivation and assignment resolution for a task plan.
//!
//! This module owns no I/O, environment, registry or store, so every rule
//! here can be checked without a running daemon. Profile existence is handed in as a predicate rather than
//! looked up, and a role name that is not in any table is not this module's
//! concern ([`PlanError`] never rejects for that reason alone) -- the
//! caller resolves role names against `roles.toml` before calling
//! [`resolve_assignment`].

use std::collections::{HashMap, HashSet, VecDeque};

use yagura_protocol::{
    TaskInstructionError, is_valid_token, validate_launch_option_value, validate_task_instruction,
    validate_window_name,
};

/// Maximum number of tasks a single plan may declare.
pub const MAX_PLAN_TASKS: usize = 256;

/// Maximum number of write targets a single task may declare.
pub const MAX_TASK_WRITES: usize = 64;

/// Maximum byte length of a normalized write-target path.
pub const MAX_WRITE_PATH_BYTES: usize = 512;

/// One task as submitted for validation, before wave numbers or resolved
/// assignments exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    /// Identifier, unique within the plan. Validated against the same
    /// token rule as a launch profile id ([`yagura_protocol::is_valid_token`]).
    pub task_id: String,
    /// Display title, validated against [`validate_window_name`].
    pub title: String,
    /// Optional role name looked up in `roles.toml` by the caller; this
    /// module only checks its token shape.
    pub role: Option<String>,
    /// Optional parent-specified launch profile id.
    pub profile: Option<String>,
    /// Optional parent-specified model. Only meaningful together with
    /// `profile` (see [`PlanError::LaunchOptionWithoutProfile`]).
    pub model: Option<String>,
    /// Optional parent-specified effort. Same profile requirement as `model`.
    pub effort: Option<String>,
    /// `task_id`s this task depends on.
    pub depends_on: Vec<String>,
    /// Raw (not yet normalized) write-target paths.
    pub writes: Vec<String>,
    /// Optional instruction text, validated against
    /// [`validate_task_instruction`].
    pub instruction: Option<String>,
}

/// A plan as submitted for validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSpec {
    /// Display title, validated against [`validate_window_name`].
    pub title: String,
    /// Tasks in registration order.
    pub tasks: Vec<TaskSpec>,
}

/// A resolved profile/model/effort triple. Always resolved as one unit: a
/// caller never mixes `model` from one source with `profile` from another,
/// since a model name is only meaningful for the profile it was chosen for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// Launch profile id.
    pub profile: String,
    /// Optional model, already validated against
    /// [`validate_launch_option_value`] when it came from a [`TaskSpec`].
    pub model: Option<String>,
    /// Optional effort, same validation as `model`.
    pub effort: Option<String>,
}

/// Where a resolved [`Assignment`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentSource {
    /// A user-set override (highest priority).
    User,
    /// The parent-supplied `profile`/`model`/`effort` on the task.
    Parent,
    /// A `roles.toml` table row, looked up by the caller.
    Role,
    /// The `roles.toml` `default_role` row, used when the task's role gave
    /// no row.
    Default,
    /// No source supplied an assignment.
    Unassigned,
}

/// The result of [`resolve_assignment`]: an [`Assignment`] (absent only
/// when `source` is [`AssignmentSource::Unassigned`]) together with the
/// source it was drawn from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAssignment {
    /// Which of the candidate sources won.
    pub source: AssignmentSource,
    /// The winning assignment, or `None` when unassigned.
    pub assignment: Option<Assignment>,
}

/// One task after successful validation: normalized writes, a computed wave
/// number, and the parent-specified assignment (if any) but not yet
/// resolved against a user override or role table -- that step is
/// [`resolve_assignment`], run by the caller once per task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTask {
    /// Identifier, unique within the plan.
    pub task_id: String,
    /// Display title.
    pub title: String,
    /// Role name, unresolved (see the module doc comment).
    pub role: Option<String>,
    /// `task_id`s this task depends on.
    pub depends_on: Vec<String>,
    /// Normalized write-target paths, in their original case.
    pub writes: Vec<String>,
    /// Longest dependency-chain length, root at 0.
    pub wave: u32,
    /// The parent-specified assignment, if `profile` was set.
    pub parent: Option<Assignment>,
    /// The task's instruction text, if any.
    pub instruction: Option<String>,
}

/// A plan that passed every check in [`validate_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPlan {
    /// Display title.
    pub title: String,
    /// Tasks, in the same order as the submitted [`PlanSpec`].
    pub tasks: Vec<ValidatedTask>,
    /// `task_id`s grouped by wave number; within a group, tasks keep their
    /// registration order.
    pub waves: Vec<Vec<String>>,
}

/// One pair of tasks whose normalized write targets collide without being
/// ordered by any dependency chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteConflict {
    /// The first task's id (validated, safe to display).
    pub task_a: String,
    /// The second task's id (validated, safe to display).
    pub task_b: String,
    /// The normalized, case-preserved path the two tasks collide on.
    pub path: String,
}

/// Why [`normalize_write_path`] rejected a raw path. Every variant is
/// dataless except the byte-count report, so a rejected path is never
/// echoed back through [`std::fmt::Display`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WritePathError {
    /// The path is empty (or reduces to nothing once `.` segments and
    /// empty segments are dropped).
    #[error("write target path is empty")]
    Empty,
    /// The path is rooted (starts with `/` after backslashes are
    /// normalized).
    #[error("write target path must be relative")]
    Absolute,
    /// The path carries a Windows drive letter (`C:`).
    #[error("write target path must not carry a drive letter")]
    DriveLetter,
    /// The path contains a `..` segment.
    #[error("write target path must not traverse outside the plan's working directory")]
    ParentTraversal,
    /// The normalized path exceeds [`MAX_WRITE_PATH_BYTES`].
    #[error("write target path is {actual} bytes, exceeding the {max}-byte limit")]
    TooLong {
        /// The limit that was exceeded ([`MAX_WRITE_PATH_BYTES`]).
        max: usize,
        /// The path's actual length in UTF-8 bytes.
        actual: usize,
    },
    /// The path contains a control character or a `"`.
    #[error("write target path contains a forbidden character")]
    ForbiddenChar,
}

/// Why [`validate_plan`] rejected a [`PlanSpec`]. Variants that could carry
/// a rejected value back to the caller instead carry nothing, so an
/// `invalid_argument` response can never echo attacker-controlled text
/// [`PlanError::Cycle`] and [`PlanError::WriteConflict`]
/// are the deliberate exception: they name only `task_id`s and normalized
/// paths that already passed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// The plan declares zero tasks.
    #[error("plan has no tasks")]
    NoTasks,
    /// The plan declares more tasks than [`MAX_PLAN_TASKS`].
    #[error("plan has {actual} tasks, exceeding the {max}-task limit")]
    TooManyTasks {
        /// The limit that was exceeded ([`MAX_PLAN_TASKS`]).
        max: usize,
        /// The plan's actual task count.
        actual: usize,
    },
    /// A plan or task title failed [`validate_window_name`].
    #[error("title is invalid")]
    InvalidTitle,
    /// A `task_id` failed [`is_valid_token`].
    #[error("task_id is invalid")]
    InvalidTaskId,
    /// The same `task_id` was declared more than once.
    #[error("task_id is duplicated within the plan")]
    DuplicateTaskId,
    /// A role name failed [`is_valid_token`].
    #[error("role name is invalid")]
    InvalidRoleName,
    /// `model` or `effort` was set without a `profile`.
    #[error("model or effort was specified without a profile")]
    LaunchOptionWithoutProfile,
    /// `profile` does not satisfy the caller's `profile_exists` predicate.
    #[error("profile does not exist")]
    UnknownProfile,
    /// `model` or `effort` failed [`validate_launch_option_value`].
    #[error("model or effort value is invalid")]
    InvalidLaunchOptionValue,
    /// `depends_on` names a `task_id` absent from this plan.
    #[error("depends_on names a task_id that is not in this plan")]
    UnknownDependency,
    /// A task lists itself in its own `depends_on`.
    #[error("a task depends on itself")]
    SelfDependency,
    /// A task's `depends_on` repeats the same `task_id`.
    #[error("depends_on repeats the same task_id")]
    DuplicateDependency,
    /// `depends_on` edges form a cycle; wave derivation and write-conflict
    /// checking do not run when this is reported.
    #[error("dependency cycle among tasks: {task_ids:?}")]
    Cycle {
        /// Every `task_id` on the cycle.
        task_ids: Vec<String>,
    },
    /// A write-target path failed [`normalize_write_path`].
    #[error("write target path is invalid")]
    InvalidWritePath,
    /// A task declares more write targets than [`MAX_TASK_WRITES`].
    #[error("task declares more write targets than the per-task limit")]
    TooManyWrites,
    /// A task declares the same normalized write target twice
    /// (ASCII case-insensitive).
    #[error("task declares the same write target twice")]
    DuplicateWrite,
    /// Two or more tasks not ordered by any dependency chain declare the
    /// same normalized write target.
    #[error("{} write-target conflict(s) between tasks not ordered by a dependency", conflicts.len())]
    WriteConflict {
        /// Every conflicting pair, reported together so the caller can fix
        /// them all in one round trip.
        conflicts: Vec<WriteConflict>,
    },
    /// A task's `instruction` failed [`validate_task_instruction`].
    #[error(transparent)]
    InvalidInstruction(TaskInstructionError),
}

/// Normalizes a raw write-target path: converts `\` to `/`, drops a leading
/// `./` and collapses repeated `/`, and removes `.` segments.
///
/// Rejects an absolute path, a Windows drive letter, a `..` segment, an
/// empty result, a result over [`MAX_WRITE_PATH_BYTES`], and any control
/// character or `"` in the raw input (both would otherwise inflate a JSON
/// encoding of the path past the byte budget it was checked against).
///
/// # Errors
///
/// Returns the first [`WritePathError`] found: forbidden character or
/// emptiness on the raw input, then drive letter, then absolute, then a
/// `..` segment, then the length limit on the normalized result.
pub fn normalize_write_path(raw: &str) -> Result<String, WritePathError> {
    if raw.is_empty() {
        return Err(WritePathError::Empty);
    }
    if raw.contains('"') || raw.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(WritePathError::ForbiddenChar);
    }

    let slashed = raw.replace('\\', "/");
    let bytes = slashed.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(WritePathError::DriveLetter);
    }
    if slashed.starts_with('/') {
        return Err(WritePathError::Absolute);
    }

    let mut segments: Vec<&str> = Vec::new();
    for segment in slashed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(WritePathError::ParentTraversal);
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        return Err(WritePathError::Empty);
    }
    let normalized = segments.join("/");

    if normalized.len() > MAX_WRITE_PATH_BYTES {
        return Err(WritePathError::TooLong {
            max: MAX_WRITE_PATH_BYTES,
            actual: normalized.len(),
        });
    }

    Ok(normalized)
}

/// Result of one pass over a dependency graph: the longest
/// dependency-chain length for every node, a dependency-first visitation
/// order, and the set of nodes a cycle kept from ever being visited.
struct Topology {
    /// Wave number per task index. Entries for indices in `unresolved` are
    /// left at `0` and must not be used by the caller.
    wave: Vec<u32>,
    /// Indices in dependency-first order: every dependency of `order[k]`
    /// that itself made it into this order appears earlier in it.
    order: Vec<usize>,
    /// Indices never visited -- exactly the tasks sitting on a cycle, or
    /// depending (directly or transitively) on one.
    unresolved: Vec<usize>,
}

/// Kahn's-algorithm pass computing [`Topology`] from `edges` (`edges[i]`
/// lists the indices task `i` depends on, self-loops already excluded).
///
/// A cycle is not treated as an error here: the nodes on it are simply
/// never dequeued and end up in [`Topology::unresolved`]. The caller
/// decides what a non-empty `unresolved` means.
fn compute_topology(n: usize, edges: &[Vec<usize>]) -> Topology {
    let mut pending: Vec<usize> = edges.iter().map(Vec::len).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, deps) in edges.iter().enumerate() {
        for &dep in deps {
            dependents[dep].push(i);
        }
    }

    let mut wave = vec![0u32; n];
    let mut processed = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| pending[i] == 0).collect();

    while let Some(i) = queue.pop_front() {
        processed[i] = true;
        order.push(i);
        for &dependent in &dependents[i] {
            wave[dependent] = wave[dependent].max(wave[i] + 1);
            pending[dependent] -= 1;
            if pending[dependent] == 0 {
                queue.push_back(dependent);
            }
        }
    }

    let unresolved: Vec<usize> = (0..n).filter(|&i| !processed[i]).collect();
    Topology {
        wave,
        order,
        unresolved,
    }
}

/// Resolves every task's `depends_on` entries to indices within `tasks`.
///
/// A self-reference or an entry naming no task in `tasks` is silently
/// dropped rather than reported: this helper has no way to report a
/// validation failure. [`validate_plan`] handles both cases itself, as
/// [`PlanError::SelfDependency`] and [`PlanError::UnknownDependency`],
/// before building its own edge list.
fn resolve_edges(tasks: &[TaskSpec]) -> Vec<Vec<usize>> {
    let index_of: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.task_id.as_str(), i))
        .collect();
    tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            t.depends_on
                .iter()
                .filter_map(|dep| index_of.get(dep.as_str()).copied())
                .filter(|&dep_index| dep_index != i)
                .collect()
        })
        .collect()
}

/// Computes each task's wave number: the length of the longest dependency
/// chain ending at it, with a dependency-free task at wave `0`. Assumes `tasks` is acyclic; a caller with unvalidated input
/// should go through [`validate_plan`] instead, which rejects cycles.
///
/// The result is in the same order as `tasks`; grouping tasks by wave
/// number in registration order is [`validate_plan`]'s job
/// ([`ValidatedPlan::waves`]).
pub fn compute_waves(tasks: &[TaskSpec]) -> Vec<u32> {
    let edges = resolve_edges(tasks);
    compute_topology(tasks.len(), &edges).wave
}

/// Transitive dependency closure for each task, built from `edges` in the
/// dependency-first `order` [`compute_topology`] already produced: by the
/// time task `i` is visited, every one of its direct dependencies' own
/// ancestor set is already complete.
fn transitive_ancestors(edges: &[Vec<usize>], order: &[usize]) -> Vec<HashSet<usize>> {
    let mut ancestors: Vec<HashSet<usize>> = vec![HashSet::new(); edges.len()];
    for &i in order {
        for &dep in &edges[i] {
            ancestors[i].insert(dep);
            let inherited: Vec<usize> = ancestors[dep].iter().copied().collect();
            ancestors[i].extend(inherited);
        }
    }
    ancestors
}

/// Validates a [`PlanSpec`] and, on success, derives its waves: task shape
/// and token rules, dependency existence/self-reference/duplication/cycles,
/// parent launch-option rules, write-target normalization and per-task
/// limits, and cross-task write conflicts.
///
/// `profile_exists` is asked about every task's `profile`, if set; this
/// module never looks a profile up itself.
///
/// # Errors
///
/// Returns every violation found, not just the first. When `depends_on`
/// edges form a cycle, [`PlanError::Cycle`] is the only dependency-graph
/// error reported: wave derivation and write-conflict checking do not run
/// since a cycle leaves "longest chain" and "ordered by a
/// dependency" undefined.
pub fn validate_plan(
    spec: &PlanSpec,
    profile_exists: impl Fn(&str) -> bool,
) -> Result<ValidatedPlan, Vec<PlanError>> {
    let mut errors: Vec<PlanError> = Vec::new();

    if spec.tasks.is_empty() {
        errors.push(PlanError::NoTasks);
    }
    if spec.tasks.len() > MAX_PLAN_TASKS {
        errors.push(PlanError::TooManyTasks {
            max: MAX_PLAN_TASKS,
            actual: spec.tasks.len(),
        });
    }
    if validate_window_name(&spec.title).is_err() {
        errors.push(PlanError::InvalidTitle);
    }

    let n = spec.tasks.len();
    let mut index_of: HashMap<&str, usize> = HashMap::with_capacity(n);
    let mut duplicate_task_id = false;
    for (i, t) in spec.tasks.iter().enumerate() {
        if !is_valid_token(&t.task_id) {
            errors.push(PlanError::InvalidTaskId);
        }
        if index_of.insert(t.task_id.as_str(), i).is_some() {
            errors.push(PlanError::DuplicateTaskId);
            duplicate_task_id = true;
        }
    }

    let mut normalized_writes: Vec<Vec<String>> = Vec::with_capacity(n);
    for t in &spec.tasks {
        if validate_window_name(&t.title).is_err() {
            errors.push(PlanError::InvalidTitle);
        }
        if let Some(role) = &t.role
            && !is_valid_token(role)
        {
            errors.push(PlanError::InvalidRoleName);
        }
        if t.model.is_some() && t.profile.is_none() {
            errors.push(PlanError::LaunchOptionWithoutProfile);
        }
        if t.effort.is_some() && t.profile.is_none() {
            errors.push(PlanError::LaunchOptionWithoutProfile);
        }
        if let Some(profile) = &t.profile
            && !profile_exists(profile)
        {
            errors.push(PlanError::UnknownProfile);
        }
        if let Some(model) = &t.model
            && validate_launch_option_value(model).is_err()
        {
            errors.push(PlanError::InvalidLaunchOptionValue);
        }
        if let Some(effort) = &t.effort
            && validate_launch_option_value(effort).is_err()
        {
            errors.push(PlanError::InvalidLaunchOptionValue);
        }
        if let Some(instruction) = &t.instruction
            && let Err(err) = validate_task_instruction(instruction)
        {
            errors.push(PlanError::InvalidInstruction(err));
        }

        if t.writes.len() > MAX_TASK_WRITES {
            errors.push(PlanError::TooManyWrites);
        }
        let mut task_writes = Vec::with_capacity(t.writes.len());
        let mut seen_lower: HashSet<String> = HashSet::with_capacity(t.writes.len());
        for raw in &t.writes {
            match normalize_write_path(raw) {
                Ok(normalized) => {
                    if seen_lower.insert(normalized.to_ascii_lowercase()) {
                        task_writes.push(normalized);
                    } else {
                        errors.push(PlanError::DuplicateWrite);
                    }
                }
                Err(_) => errors.push(PlanError::InvalidWritePath),
            }
        }
        normalized_writes.push(task_writes);
    }

    // Dependency edges: only entries naming an existing, non-self task,
    // deduplicated within the task. A self-reference, a repeated entry or a
    // dangling reference is rejected outright rather than folded into the
    // graph.
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut graph_broken = duplicate_task_id;
    for (i, t) in spec.tasks.iter().enumerate() {
        let mut seen_deps: HashSet<&str> = HashSet::with_capacity(t.depends_on.len());
        for dep in &t.depends_on {
            if dep == &t.task_id {
                errors.push(PlanError::SelfDependency);
                graph_broken = true;
                continue;
            }
            if !seen_deps.insert(dep.as_str()) {
                errors.push(PlanError::DuplicateDependency);
                graph_broken = true;
                continue;
            }
            match index_of.get(dep.as_str()) {
                Some(&dep_index) => edges[i].push(dep_index),
                None => {
                    errors.push(PlanError::UnknownDependency);
                    graph_broken = true;
                }
            }
        }
    }

    if graph_broken {
        return Err(errors);
    }

    let topo = compute_topology(n, &edges);
    if !topo.unresolved.is_empty() {
        let mut task_ids: Vec<String> = topo
            .unresolved
            .iter()
            .map(|&i| spec.tasks[i].task_id.clone())
            .collect();
        task_ids.sort();
        errors.push(PlanError::Cycle { task_ids });
        return Err(errors);
    }

    let ancestors = transitive_ancestors(&edges, &topo.order);

    let mut by_path: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for (i, writes) in normalized_writes.iter().enumerate() {
        for path in writes {
            by_path
                .entry(path.to_ascii_lowercase())
                .or_default()
                .push((i, path.clone()));
        }
    }
    let mut keys: Vec<&String> = by_path.keys().collect();
    keys.sort();

    let mut conflicts: Vec<WriteConflict> = Vec::new();
    for key in keys {
        let owners = &by_path[key];
        for (offset, (i, path_i)) in owners.iter().enumerate() {
            for (j, _) in owners.iter().skip(offset + 1) {
                let ordered = ancestors[*i].contains(j) || ancestors[*j].contains(i);
                if !ordered {
                    conflicts.push(WriteConflict {
                        task_a: spec.tasks[*i].task_id.clone(),
                        task_b: spec.tasks[*j].task_id.clone(),
                        path: path_i.clone(),
                    });
                }
            }
        }
    }
    if !conflicts.is_empty() {
        errors.push(PlanError::WriteConflict { conflicts });
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let mut grouped: Vec<Vec<usize>> = Vec::new();
    for (i, &wave) in topo.wave.iter().enumerate() {
        let wave = wave as usize;
        if grouped.len() <= wave {
            grouped.resize_with(wave + 1, Vec::new);
        }
        grouped[wave].push(i);
    }
    let waves: Vec<Vec<String>> = grouped
        .into_iter()
        .map(|indices| {
            indices
                .into_iter()
                .map(|i| spec.tasks[i].task_id.clone())
                .collect()
        })
        .collect();

    let tasks: Vec<ValidatedTask> = spec
        .tasks
        .iter()
        .enumerate()
        .map(|(i, t)| ValidatedTask {
            task_id: t.task_id.clone(),
            title: t.title.clone(),
            role: t.role.clone(),
            depends_on: t.depends_on.clone(),
            writes: normalized_writes[i].clone(),
            wave: topo.wave[i],
            parent: t.profile.clone().map(|profile| Assignment {
                profile,
                model: t.model.clone(),
                effort: t.effort.clone(),
            }),
            instruction: t.instruction.clone(),
        })
        .collect();

    Ok(ValidatedPlan {
        title: spec.title.clone(),
        tasks,
        waves,
    })
}

/// Resolves a task's profile/model/effort assignment: the user override
/// wins if present, then the parent specification, then the role-table
/// row, then the table's default row, each taken as a whole triple rather
/// than field by field. The role table itself is not this function's
/// concern -- the caller passes `None` for `role` / `default` whether the
/// table lookup found nothing or the table was unusable, and either way this
/// resolves to [`AssignmentSource::Unassigned`] when nothing else won.
pub fn resolve_assignment(
    user: Option<&Assignment>,
    parent: Option<&Assignment>,
    role: Option<&Assignment>,
    default: Option<&Assignment>,
) -> ResolvedAssignment {
    if let Some(assignment) = user {
        return ResolvedAssignment {
            source: AssignmentSource::User,
            assignment: Some(assignment.clone()),
        };
    }
    if let Some(assignment) = parent {
        return ResolvedAssignment {
            source: AssignmentSource::Parent,
            assignment: Some(assignment.clone()),
        };
    }
    if let Some(assignment) = role {
        return ResolvedAssignment {
            source: AssignmentSource::Role,
            assignment: Some(assignment.clone()),
        };
    }
    if let Some(assignment) = default {
        return ResolvedAssignment {
            source: AssignmentSource::Default,
            assignment: Some(assignment.clone()),
        };
    }
    ResolvedAssignment {
        source: AssignmentSource::Unassigned,
        assignment: None,
    }
}
