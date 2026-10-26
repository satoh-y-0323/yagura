# Tickets

This is the specification for yagura's ticket board. The tool-by-tool reference (arguments, reply
shapes and every error code) is in [docs/mcp-setup.md](mcp-setup.md#list_tickets); this document
describes what the board is for and the rules behind it.

## What the ticket board is for

A **ticket** is one unit of work an agent and you keep track of: a title, a body, a state, and the
results recorded while it is worked on. Tickets belong to a **workspace** -- the git repository a
window was opened in, or the directory itself when it is outside a repository -- so every window
opened in the same repository sees the same board, and no other repository's. Numbers (`T-1`,
`T-2`, ...) start at 1 in each workspace and are never reused. A ticket is never deleted: a ticket
that will not be done is moved to a state that says so.

yagura carries no workflow of its own. The states a ticket can be in, and the rules for using them,
are yours: you write them in `tickets.toml` (below). yagura enforces exactly one thing -- a state
may **require** fields, and a ticket cannot be written into such a state without a value for each
of them. Every other rule (how many tickets may be in progress, when a ticket may be marked done)
is text yagura shows the agent as the board's rules; the agent follows it or does not, and yagura
does not check.

A window's agent reads and writes the board with MCP tools
([`list_tickets`](mcp-setup.md#list_tickets), [`get_ticket`](mcp-setup.md#get_ticket),
[`create_ticket`](mcp-setup.md#create_ticket), [`update_ticket`](mcp-setup.md#update_ticket),
[`add_ticket_result`](mcp-setup.md#add_ticket_result), [`set_milestone`](mcp-setup.md#set_milestone)).
A window can only reach its own workspace's tickets, and cannot name another workspace. The GUI is
read-only: it shows the board and lets you open a ticket, but has no edit button.

## The board's rules: `tickets.toml`

```toml
schema = 1

guide = """
One ticket, one purpose: when work has two goals, split it into two tickets.
"""

[[state]]
name = "todo"
description = "Decided to do, not started yet."
requires = []

[[state]]
name = "on-hold"
description = "Paused. Say what brings it back."
requires = ["resume_trigger"]

[[field]]
name = "resume_trigger"
description = "The event or condition that brings this ticket back."
```

- `schema` must be `1`. Unknown keys are rejected.
- `guide` is free text for the agent: the board's rules in your own words.
- Each `[[state]]` has a `name`, a `description` and `requires`, the names of the fields a ticket in
  that state must have a value for. **The order of the `[[state]]` tables is the order of the
  board's columns, and the first one is the state a new ticket starts in** when the agent names
  none.
- Each `[[field]]` has a `name` and a `description`. A ticket's fields are the ones defined here;
  a field not defined is refused.
- State and field names use 1 to 64 bytes of `a`-`z`, `0`-`9`, `-` and `_`, and must be unique. A
  `requires` that names an undefined field is rejected. At least one state, at most 32 states and
  32 fields, and the file is at most 64 KiB.
- yagura does not look at what a state means. It does not treat `done` or `cancelled` differently
  from any other state.

An empty or whitespace-only value does not satisfy a requirement. When an update changes a ticket's
state or its fields, the state the ticket ends in is checked against the fields after the change,
so a ticket cannot sit in `on-hold` with its `resume_trigger` cleared either. A change to only the
title, body or milestone checks nothing, so adding a requirement later does not block unrelated
edits. A refusal is `invalid_argument` and names the missing fields; nothing is written.

The daemon finds the file in this order:

1. The `--tickets <path>` command-line option.
2. The `YAGURA_TICKETS` environment variable. An empty value counts as unset.
3. `tickets.toml` in the config root (see [docs/instances.md](instances.md)).

The GUI places a reference `tickets.toml` in the config root the first time it starts, and never
overwrites a file that is already there. The daemon reads the file again at every request, so an
edit takes effect from the next request with no restart (the GUI's board shows it the next time
the Plans panel is opened or a ticket changes). One file serves every workspace of the instance.

While the file is missing or cannot be read, `create_ticket` and `update_ticket` are refused with
`tickets_not_configured` or `tickets_config_invalid` (the message says why and never quotes the
file). Reading keeps working: `list_tickets` returns the summaries with `config_error` in place of
`config`, and `add_ticket_result` and `set_milestone` do not depend on the file.

### The reference settings

The file the GUI places has six states, in this order:

| State | Meaning | Requires |
|---|---|---|
| `todo` | Decided to do, not started yet | -- |
| `in-progress` | Being worked on now | -- |
| `done` | Finished, with its results recorded | -- |
| `considering` | Not decided yet | `start_condition` |
| `on-hold` | Paused | `resume_trigger` |
| `cancelled` | Will not be done | `reason` |

Its `guide` asks for one purpose per ticket, one or two tickets in progress at a time, deferred
work kept in `considering` or `on-hold` rather than dropped, results recorded as soon as they are
known and before a ticket is marked `done`, and cancellation with a reason in place of deletion.
Those are requests to the agent, not checks; only the three required fields are enforced. Edit the
file freely.

## What the agent reads, and how it is told to treat it

`list_tickets` is the call to make first. It returns the board's rules -- the `guide` and the
description of each state and field -- which are the rules you set, and the agent is told to follow
them. A ticket's title, body, field values, results and a milestone's title and description are
written by whoever works on the board, and the tools tell the agent to treat them as data, not
instructions: information to consider, never commands to obey.

A summary in the list carries the ticket's number, title, state, milestone, `updated_at`,
`plan_count` (how many plans were made from it) and `fields` (the values of the fields its current
state requires, so a card can show them without reading the ticket). A value longer than 256
characters is cut to its first 256 and ends in `…`; reading the ticket returns it whole. A ticket
whose state the settings no longer define is still listed and read, with its state as stored.

## Milestones

A milestone is a named group of a workspace's tickets, created and changed with `set_milestone`. A
ticket names at most one, and only a milestone that exists in its workspace. Milestones sort by
`position`, lowest first, equal positions by name; a list is ordered by its ticket's milestone in
that order (tickets without one after them), then by number.

## From a ticket to a plan

`create_plan` takes an optional `ticket`, the number of a ticket in the calling window's
workspace. The plan then records where it came from: [`get_plan`](mcp-setup.md#get_plan) reports
the ticket, `get_ticket` lists the plan's id among its `plans`, and the GUI's ticket view links to
it. One ticket may have any number of plans. Deleting a plan removes it from the ticket's list; the
ticket stays. **Making a plan never changes the ticket's state**: the agent moves a ticket only by
calling `update_ticket`. A number that does not exist, or that belongs to another workspace, is
refused as `ticket_not_found`, and no plan is created. See [Plans](plans.md).

## Limits

| Limit | Value |
|---|---|
| Workspaces with tickets, in the whole daemon | 256 |
| Tickets in one workspace | 10,000 |
| Results on one ticket | 256 |
| Milestones in one workspace | 64 |
| Ticket body | 16384 bytes |
| Field value | 2048 bytes |
| Result | 4096 bytes |
| Milestone description | 2048 bytes |
| States / fields in `tickets.toml` | 32 / 32 |
| `tickets.toml` | 64 KiB |
| `list_tickets` `limit` | default 200, at most 1000 |

Body, field values, results and milestone descriptions may span several lines; newlines and tabs
are allowed, and no other control characters and no characters that could reorder or hide text.
Titles are one line under the same rule as a window name. Reaching a count limit refuses the write
with `resource_exhausted`, and nothing is written. Results are append-only: each gets the next
sequence number and the daemon's own time, and none can be edited or removed.

## In the app

The Plans panel opens on its **Tickets** tab, the first of Tickets, Board, Roles and Models.
Choose a workspace (by name and path) and, optionally, a milestone. The board has one column per
state of `tickets.toml`, in its order, 200 px wide and scrolling sideways when they do not fit. A
ticket whose state the settings do not define is shown in an extra column after them, so nothing
is hidden. Each column loads its own tickets, and one that holds more than was loaded says so at
its foot. A card shows the number, title, the fields its state requires, its milestone and how
many plans were made from it; choose it to read the ticket in the tab: description, fields,
results in the order they were recorded, and its plans (choose one to open it on the Board tab).
While the tab is shown, the view updates when an agent changes the board or makes or deletes a plan
from a ticket; showing the tab again reads the workspaces and the board anew. With no usable
settings, the tab says why and shows no columns.

Opening the panel on the Tickets tab does not count as having looked at a plan: a plan's new and
changed task marks (see [Confirming a plan](plans.md#confirming-a-plan)) are cleared only when the
Board tab is shown.

Text from tickets is drawn as plain text, with control characters and characters that could
reorder or hide text made visible. A workspace's path is shown with its control characters removed
and the characters that could reorder it made visible.

## Moving a repository

A workspace is identified by the repository's path. If you move a repository's directory, windows
opened in the new place see a new, empty board: the old tickets stay with the old path. The GUI
still lists the old workspace with its path, but yagura has no way to carry tickets over to the new
location.

## Updating

Tickets live in the daemon's database. The version that introduced them moved the database to
schema 016 and the IPC protocol to revision 29, and a database opened by it cannot be opened by an
older daemon. Run the GUI and the daemon from the same release.
