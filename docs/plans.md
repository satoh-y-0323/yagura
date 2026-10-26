# Plans

This is the single specification for yagura's plan ledger. The tool-by-tool reference (arguments,
reply shapes and every error code) is in [docs/mcp-setup.md](mcp-setup.md#create_plan); this
document describes what the ledger is for and the rules behind it.

## What the plan ledger is for

A window running an agent CLI (the *parent window*) can hand yagura a **plan**: a title and a list
of tasks, each with the task ids it depends on, the paths it writes, and optionally a role or a
launch profile. yagura stores the plan, derives which tasks may run side by side (the **waves**),
checks that no two tasks that could run at the same time write the same file, and opens a window
for a task with the launch profile, model and effort assigned to it.

yagura carries no workflow of its own. It does not know what a "design" or a "review" task is, does
not decide which role a task needs, and never starts a task by itself: the parent window decides
what the tasks are and when to open each one. Role names are opaque strings the operator defines in
`roles.toml`; yagura only looks them up.

## Plans made from a ticket

`create_plan` may name a ticket of the calling window's workspace (its `ticket` argument, the number
shown as `T-<n>`). The plan then records where it came from: `get_plan` reports the `ticket`, and
the ticket's `get_ticket` lists the plan's id. Several plans may come from one ticket. The link is
only a record: creating a plan never changes the ticket's state, and deleting the plan removes it
from the ticket's list without touching the ticket. A number the workspace does not hold, whether
it does not exist or belongs to another workspace, is refused as `ticket_not_found` and creates no
plan. See [Tickets](tickets.md) for the board itself.

## Waves

A task's wave is the length of the longest dependency chain that reaches it: a task with no
dependencies is in wave 0, a task depending only on wave-0 tasks is in wave 1, and so on. Tasks in
the same wave do not depend on each other, directly or transitively, and can run in parallel. Within
a wave, tasks keep the order in which the plan listed them.

A plan whose dependencies form a cycle is rejected (`invalid_argument`, naming every task the cycle
leaves without a wave), as is a task that depends on itself, repeats the same dependency, or
depends on a task id the plan does not declare.

## Write targets and conflicts

Each task lists the paths it writes, relative to the plan's working directory (the working directory
of the window that created the plan). Before comparing, yagura normalizes each path: `\` becomes
`/`, empty and `.` segments are dropped, and the result keeps its original case. A path is rejected
when it is absolute, carries a drive letter, contains a `..` segment, is empty after normalizing,
is longer than 512 bytes, or contains a control character or `"`.

Two tasks may write the same path only when one of them depends on the other, directly or
transitively; otherwise the plan is rejected, and the error names every colliding pair of tasks
with the path they share, so the parent can fix them all at once. The same path listed twice in
one task is rejected too.

Paths are compared **without regard to ASCII letter case**. The default file systems on Windows and
macOS do not distinguish `Src/lib.rs` from `src/lib.rs`, so treating them as different paths would
let two parallel tasks write the same file. On a case-sensitive file system this can only reject a
plan that would have been safe, never admit one that was not.

## Assignments

Every task resolves to one launch profile, with an optional model and effort. The three are always
taken together from a single source -- a model name only means something for the profile it was
chosen for, so yagura never mixes a profile from one source with a model from another. The sources,
in order of priority:

1. **User** -- an override the operator set for this task in the GUI.
2. **Parent** -- the `profile` / `model` / `effort` the parent window put on the task. `model` and
   `effort` are accepted only together with `profile`.
3. **Role** -- the `roles.toml` row named by the task's `role`.
4. **Default** -- the `roles.toml` row named by `default_role`, used only when the task names no
   role at all, or names one the table has no row for (see [The role table](#the-role-table-rolestoml)).

When none of the four applies, the task is **unassigned**, and opening a window for it is refused
(`invalid_state`) until the operator sets an override, or the task (or the role table) gains one.
A `role` that names no row in the table contributes nothing on its own; it falls through to the
default instead of being an error.

## Who can write what

- **Any live window can create a plan.** The window that creates it becomes the plan's **owner**. A
  window that is no longer alive cannot create one (`invalid_state`).
- **Only the owner window, while it is alive, can change the plan**: replace its tasks
  (`update_plan`), delete it (`delete_plan`) and open a window for one of its tasks
  (`open_task_window`). Every other window gets `not_plan_owner`, or `plan_not_found` if it has no
  relation to the plan at all.
- **A task's window can write that task's report and nothing else** (see
  [Task reports](#task-reports)), and only while the task is still bound to it.
- **The operator can hand a plan to another window** from the GUI. This works whether or not the
  current owner is still alive, and the window named must itself be alive.
- **A daemon restart clears every plan's owner**, and every task's link to the window opened for
  it: window ids do not survive a restart, so a plan never keeps pointing at a window that no longer
  exists. The plans themselves, their tasks and the operator's overrides are kept. A plan left
  without an owner stays read-only to agents until the operator names a new one.
- **The GUI** can also set or clear a task's override, delete any plan, and edit the role table.
  Agents cannot do any of these.

Updating a plan replaces its whole task list. A task kept under the same task id keeps its override
and the window opened for it; a task left out loses both.

## Confirming a plan

When the playbook's `[plans] require_approval` is on -- the default, and what it stays at when the
key or the whole `[plans]` table is left out -- a plan starts out **pending**: `open_task_window` is
refused for every one of its tasks (`plan_not_approved`) until an approver settles it from the GUI.
Creating, replacing (`update_plan`) and deleting a plan are never refused on this account; the gate
sits only on opening a task's window. `create_plan` and `list_plans` report a pending plan's
`review` as `"pending"`; a plan created while `require_approval` was off carries no `review` at all,
and stays that way for its whole life -- turning the setting back on does not retroactively gate a
plan that already exists (see [docs/mcp-setup.md](mcp-setup.md#playbooks-approval-and-limits)).

Once an approver approves it, `review` becomes `"approved"`, a one-line notice is handed to the
owner window's CLI (see [How the daemon delivers text](mcp-setup.md#how-the-daemon-delivers-text)) --

```
[yagura plan] Plan <plan_id> was approved. You can open its tasks now.
```

-- and every one of the plan's tasks can be opened from then on. **A parent window waiting on a
pending plan should wait for this notice, or poll `get_plan`'s `review`, rather than retry
`open_task_window` in a loop or fall back to opening the task's work some other way** -- `open_window`
and a `send_handoff` that opens a new window are ordinary, unrelated ways to start a process, not
part of the plan mechanism, and yagura does not stop either of them just because a plan is pending
(see [Scope of the gate](#scope-of-the-gate)).

**Changes after approval are not blocked, only marked.** `update_plan` on an approved plan keeps
`open_task_window` working for every task, and additionally marks each one's `review_mark`: `"new"`
for a task id the plan did not have before, `"changed"` for one whose title, role, parent
assignment, dependencies, write targets or instruction now differs, and no mark for one that is
otherwise the same as it was. A plan with any marked task reports `unseen_changes: true` in
`list_plans`, which the GUI shows as a sidebar notice; opening the Plans board and looking at the
plan (`mark_plan_seen`) clears every mark and the notice together, but only when the version passed
back matches the plan's current one -- a mismatch (something changed again since it was read) leaves
the marks in place and returns the plan as it currently stands, so a stale acknowledgement can never
erase a change nobody has looked at yet.

**A daemon restart does not disturb any of this**: `review`, `version` and every task's
`review_mark` are stored with the plan and survive it, the same as the plan's tasks.

### Scope of the gate

The guarantee above covers exactly one thing: **binding a plan's task to a window through
`open_task_window`**. Before a pending plan is approved, that binding does not happen. Everything
else a window can already do -- open an arbitrary window with `open_window`, hand work to one with
`send_handoff` -- sits outside the plan mechanism entirely and is not stopped by it; yagura advises
against using either as a way around a pending plan (see the notice text above and
[docs/mcp-setup.md](mcp-setup.md#error-handling)'s `plan_not_approved`) but does not prevent it. Nor
does the gate defend against a parent window that deliberately works around it, or against the same
operator rewriting the playbook or the session database directly: both already run with the
operator's own privilege, which the gate was never meant to constrain. The Roles tab's own unsaved
edits are unrelated too -- it has its own Save action, and a plan's confirmation only ever looks at
the cards on the Board tab.

## Who can read what

A window can read a plan when it is the plan's owner or when it was opened for one of the plan's
tasks. To any other window the plan does not exist: `get_plan` answers `plan_not_found` -- exactly
what an unknown plan id gets -- and `list_plans` leaves it out. The GUI reads every plan.

## When a window counts as alive

A window is alive when it has not been closed and its child process has not exited. A window that
was closed, or whose process exited on its own, is not alive even though it may still appear in the
window list for a while.

The daemon decides every permission above from its own window registry at the moment of the
request. The plan itself records only window ids, never whether those windows are alive; the GUI's
Plans panel works out which owner and task windows are alive -- and which windows it can offer when
choosing a new owner -- from its own window list. That list is refreshed periodically, so the panel
can lag behind a window closing for a moment, but that never changes what the daemon allows.

A task whose window is still alive cannot be opened again (`invalid_state`). Once that window is
closed or its process exits, opening the task again starts a fresh window and replaces the link.

## Task instructions

A task can carry an `instruction`: the request the parent has for the window opened for that task,
in as many lines as it needs. yagura never hands the request itself to that window.
It stores it with the task, starts the window with a short fixed notice as its first request, and
the window reads the request back with `get_plan`. This is how a parent hands a window a multi-line request:
`send_message` refuses text with a line break anywhere but at its very end (see
[docs/mcp-setup.md](mcp-setup.md#send_message)).

- **The instruction is set with `create_plan` and `update_plan`**, and shown by `get_plan` (the
  task's `instruction`). `update_plan` always replaces it with the value the new task list gives,
  so leaving it out of a kept task clears it. It is kept across a daemon restart, like the rest of
  the task.
- **It is 1 to 16384 bytes** (`TASK_INSTRUCTION_MAX_BYTES`). Newlines and tabs are allowed; any other
  control character, and any character that could reorder or hide text, is rejected as
  `invalid_argument`. `yagura-mcp` converts `\r\n` and lone `\r` line endings to `\n` before sending
  it.
- **A task with an instruction needs a profile that can be handed a request at launch.** When the
  resolved assignment's profile declares no `launch_prompt` in its `[delivery]` table (see
  [profiles/README.md](../profiles/README.md#delivery)), `open_task_window` is refused as
  `no_composer` before any child is started, and the task is not bound. When the profile's program
  is a Windows `.cmd` / `.bat` file, through which the notice cannot pass unchanged, it is refused as
  `unsupported`. A task with no instruction still opens under any profile, since nothing needs to be
  handed over. See [docs/mcp-setup.md](mcp-setup.md#error-handling).
- **Otherwise, `open_task_window` replies at once and leaves the notice to the daemon.** For a task
  with an instruction, it opens the window, binds it to the task and replies, without waiting for the
  window. The window is started with this line as the CLI's first request:

  ```
  [yagura task] Task <task_id> of plan <plan_id> is assigned to this window. Call get_plan with plan_id "<plan_id>" and task_id "<task_id>" to read its instruction.
  ```

  Only the plan id and the task id vary; nothing from the instruction appears in the notice.
- **The window reads its request with `get_plan`, narrowed by `task_id`.** A task window can read
  its own plan (see [Who can read what](#who-can-read-what)); passing `task_id` keeps the reply to
  its own task, so it does not also receive every other task's instruction.
- **The chat announcement does not compete with the notice.** When the window also gets the chat
  intro (the playbook's `[chat].intro`, see
  [docs/mcp-setup.md](mcp-setup.md#playbooks-approval-and-limits)), the CLI reads it from its
  start-up hook, while the notice is its first request.
- **The notice does not depend on the connection that called `open_task_window`**, which may close
  at any time: it is part of how the window is started.
- **`get_plan` shows where the notice stands**, as the task's `instruction_notice`, an object of
  `state` and `at`:
  - `pending` -- the notice has not been handed over yet; `at` is `null`.
  - `delivered` -- the window was started with the notice; `at` is when, in Unix epoch seconds. It
    says the CLI was given the line, not that the window acted on it.
  - `window_closed` -- the window exited or was closed, or the daemon restarted, before the notice
    was handed over; `at` is when that was recorded, in Unix epoch seconds.

  `instruction_notice` is left out for a task with no instruction and for one whose window has
  never been opened. Opening the task's window again starts it over at `pending`.
- **The window is not closed when the notice does not arrive.** Whatever `instruction_notice`
  says, the window stays open and bound to its task. The instruction stays readable with
  `get_plan`; to prompt the window, send it a one-line `send_message` telling it to call `get_plan`
  (which goes through the approval gate like any other `send_message`).
- **Changing an instruction does not notify a window already open for the task.** yagura has no
  separate path for handing an open window a second multi-line request: change the instruction with
  `update_plan`, then prompt the window with a one-line `send_message` to read it again, or open the
  task's window again once the old one has closed.

Instructions are kept in the session store in plain text; do not put secrets in them.

## Task reports

A window opened for a task reports how its work ended with `send_report`: an outcome
(`succeeded` or `failed`), a one-line summary and, optionally, the path of a file holding a longer
result. yagura does not decide on its own whether a task is finished; the task's report is the
only record of that, and it is only as reliable as the window that wrote it.

- **The report is recorded against the task in the ledger** and shown by `get_plan` (the task's
  `report`) and on the GUI's Plans board. A later report from the same window replaces the earlier
  one.
- **It is also handed to the plan's owner window**, which collects it with `wait_for_report`. When
  the owner is not waiting, the daemon hands the owner's CLI a one-line notice. When the plan has no live owner, the report is still recorded. See
  [docs/mcp-setup.md](mcp-setup.md#send_report) for the delivery rules and limits.
- **Opening the task's window again clears its report**: `open_task_window` on a task starts a new
  attempt, so the previous attempt's report no longer describes the task. Dropping the task with
  `update_plan`, or deleting the plan, removes the report with it. A task kept by `update_plan`
  keeps its report.
- **A daemon restart keeps every report.** The restart clears each task's window link, but a task
  that was reported done stays reported done.
- **Only the window the task is currently bound to can report on it.** Once the task has been
  opened again in another window, dropped by `update_plan`, or its plan deleted, a report from the
  old window is refused (`invalid_state`) and is neither recorded nor delivered, so an earlier
  attempt's outcome can never be taken for the current one.

Reports are kept in the session store in plain text; do not put secrets in them.

## Plans that nobody owns

yagura never deletes a plan by itself and keeps no expiry date. A plan whose owner is gone stays in
the ledger until the operator deletes it or hands it to another window. Which plans are still worth
keeping is the operator's call.

## The role table: `roles.toml`

`roles.toml` maps a role name to a launch profile and, optionally, a model and an effort:

```toml
schema = 1
default_role = "light"

[[role]]
name = "heavy"
profile = "claude-code"
model = "strong"
effort = "high"

[[role]]
name = "light"
profile = "codex-cli"
```

- `schema` must be `1`. Unknown keys are rejected.
- `name` uses the same rule as a task id: 1 to 64 bytes of lowercase ASCII letters, digits, `-` and
  `_`. Names must be unique within the table.
- `model` and `effort` follow the launch option value rule (see
  [docs/mcp-setup.md](mcp-setup.md#error-handling)). When `model` names one of the profile's
  aliases (such as `strong` above), the model it stands for is the alias's value in that profile,
  which the Models tab of the GUI's Plans panel changes without touching this table (see
  [profiles/README.md](../profiles/README.md#editing-aliases-in-the-gui)).
- `default_role` (optional) names the row a task falls back to when it names no role at all, or
  names one this table has no row for -- see [Assignments](#assignments) for where it sits in the
  resolution order. Naming a row the table does not have is rejected the same way an invalid row is,
  on both read and save, so the table is either fully consistent or fully unusable; it is never
  silently dropped.
- A single invalid row, or a `default_role` naming no row in the table, makes the whole table
  unusable rather than being skipped. While the table is unusable, `list_roles` answers
  `invalid_state`, plans still load but carry a `roles_warning`, and no task's assignment comes from
  a role or from the default.
- A profile the user has switched off (see
  [profiles/README.md](../profiles/README.md#disabling-a-profile)) still counts as existing for the
  save check below: a role naming a disabled profile saves and loads exactly like any other row, and
  only opening a window through it is refused, as `profile_disabled`
  ([docs/mcp-setup.md](mcp-setup.md#open_task_window)).

The daemon finds the file in this order:

1. The `--roles <path>` command-line option.
2. The `YAGURA_ROLES` environment variable. An empty value counts as unset.
3. `roles.toml` in the config root (see [docs/instances.md](instances.md)).

yagura does **not** create `roles.toml` on first run and ships no default roles. A missing file
reads as an empty table. The operator creates the first roles from the Roles tab of the GUI's Plans
panel; saving writes the file at the resolved location, and is refused (`unsupported`) when no
location could be resolved at all. Saving checks that every row's profile exists; reading does not,
so a table keeps working after one of the profiles it names is removed (a task resolving to that
profile is then refused when its window is opened).

## Limits

| Limit | Value |
|---|---|
| Plans held by the daemon | 64 |
| Plans owned by one window (checked when it creates a plan) | 16 |
| Tasks in one plan | 256 |
| Dependencies of one task | 255 (one fewer than the task limit) |
| Write targets of one task | 64 |
| Length of one write target | 512 bytes |
| Task instruction | 1 to 16384 bytes; newlines and tabs allowed, no other control characters and no characters that could reorder or hide text |
| Rows in `roles.toml` | 64 |
| Plan and task titles | the window name rule: 1 to 128 bytes, no forbidden characters |
| Task ids and role names | 1 to 64 bytes of `a`-`z`, `0`-`9`, `-`, `_` |

There is no separate limit on dependencies: a task can depend on every other task in the plan and
no more, since a task may not depend on itself or name the same dependency twice. The per-window
plan limit counts every plan the window currently owns, including plans the operator handed to it,
but is only checked when the window creates a plan; the operator can always hand a plan to a
window. Reaching either plan limit refuses creation with `resource_exhausted`.

## Using the ledger from a parent window

- Call `list_roles` first to see which role names the operator has defined, and put `role` on a
  task when one fits; add `profile` (and `model` / `effort`) only when the task needs something the
  roles do not cover. The operator can override either from the GUI.
- Split work so tasks that write the same file depend on one another. If `create_plan` reports a
  write conflict, either add the missing dependency or move the shared file into one task.
- Open a task's window with `open_task_window` once the tasks it depends on are done. The waves
  only say what may run side by side; whether a task is done is what its report says (see
  [Task reports](#task-reports)). The window is named after the task's title unless you pass
  `name`.
- Call `wait_for_report` to collect the reports of the task windows you opened, and whenever the
  `[yagura report]` notice reaches your CLI.
- Put a request that spans more than one line in the task's `instruction` rather than sending it
  with `send_message`, and check the task's `instruction_notice` in `get_plan` to see whether the
  window has been told about it (see [Task instructions](#task-instructions)).
- Use `get_plan` to read the current assignments before opening a window: the operator may have
  changed them since the plan was created.
- Delete a plan with `delete_plan` when it is finished, so it does not count toward the limits.
