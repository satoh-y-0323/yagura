# Detection definitions

This directory holds the state-detection definitions the yagura daemon matches
PTY output against: one TOML file per CLI, named `<name>.toml`.

A definition says how to *read* that terminal: the patterns and clocks
below, and, in its `[composer]` table, where the CLI draws its input area, so
that what is being typed there never decides the window's status. The daemon
never types into a window: what a window is sent reaches its CLI through the
CLI's own doors (see
[How the daemon delivers text](../../../docs/mcp-setup.md#how-the-daemon-delivers-text)).

Definitions are data, not code. When a CLI changes its TUI, only the file
here should need editing -- the daemon knows nothing about any particular
vendor, and vendor identity exists only as data (the file name and the `name`
key). The files here are compiled into `yagura-daemon` as its bundled layer,
so a change to one of them travels with the next build of the daemon; a
definition on disk (see [Selecting a definition](#selecting-a-definition))
needs no rebuild.

Definitions in this directory:

- `claude-code` -- Claude Code.
- `codex-cli` -- Codex CLI.
- `antigravity-cli` -- Antigravity CLI (`agy`).
- `copilot-cli` -- GitHub Copilot CLI (`copilot`).
- `muse-code` -- Muse Code (`muse`).
- `grok-build` -- Grok Build (`grok`).
- `generic-shell` -- a plain interactive shell, and the daemon's built-in
  default when nothing selects a definition. It declares no `[composer]`: a
  plain shell's screen alone cannot tell its prompt apart from a program's
  own output.

Adding a definition to this directory means adding it to that list too.

`generic-shell` reads `$`, `#`, `>` or `%` followed by a space as the end of
its prompt (zsh draws `~ % ` or `host% `), so a symbol with nothing after it,
such as the figure in `45%`, is not read as one. Its `idle` rule takes a `%`
only after whitespace; a prompt like `host% ` turns `idle` once output has
gone quiet. A line from a running program that ends in one of these symbols
and a space (`Downloading 45% `, `Enter value> `) cannot be told apart from
an empty prompt.

A definition name is not a vendor key. `codex-cli` names the file here whose
patterns a window's output is matched against; `codex` is the `vendor` value
that same window carries for theming (see the
[Theme Guide](../../../docs/themes.md#vendor-assignments)). The two are set
independently, and neither implies the other.

One note specific to Codex CLI: the npm-installed `codex` is a `.cmd` shim on
Windows. The daemon wraps such a shim through `cmd.exe /d /c` automatically
when resolving a window's or profile's command, so naming the bare `codex`
command works without an absolute path or a manual `cmd /c` prefix. A shim
path or argument containing a command-interpreter metacharacter (`%` `&` `|`
`<` `>` `^` `"`, or a newline) is rejected instead of being passed through --
see [`.cmd` / `.bat` shims](../../../profiles/README.md#cmd--bat-shims) for the
full rule. The bundled `codex-cli` profile nonetheless needs Codex CLI's
standalone build on Windows, because the daemon wakes a waiting Codex CLI with
a command that passes text as an argument (see
[Delivery](../../../profiles/README.md#delivery)).

## Selecting a definition

Each window loads exactly one definition; definitions are never merged for a
single window. `--detection` sets the daemon-wide default, applied to windows
that do not choose one of their own; an individual window can select a
different definition per-window, at open time, via the IPC `open_window`
message's `detection` field or the MCP `open_window` tool's `detection`
argument (see [MCP Setup Guide](../../../docs/mcp-setup.md#open_window)).

A [launch profile](../../../profiles/README.md) is a third way to select a
definition: its own `detection` key is applied when the window is opened
from that profile, the same way the `detection` field/argument above is. A
profile launch does not accept `detection` as a separate override (the two
are mutually exclusive) -- to open a profile's command under a different
definition, edit the profile itself or open it as a plain command instead.

Whichever way a name is chosen, it is resolved against two layers.

The **bundled layer** is the set of definitions in this directory, compiled
into `yagura-daemon` when it is built. They carry the same version as the
daemon that serves them and travel inside the executable, so no packaging,
install or upgrade step can leave a window matching against a definition
older than the daemon reading it.

The **directory layer** is a single directory of `<name>.toml` files on
disk -- the user layer, where definitions you add or override yourself live.
It is resolved once at startup:

1. `--detection-dir <path>`
2. the `YAGURA_DETECTION_DIR` environment variable (an empty value counts as
   unset)
3. `<config root>/detection`, adopted only when it already exists as a
   directory (an empty one still counts)

The config root is the same one `playbook.toml` lives in:

- Windows: `%LOCALAPPDATA%\io.github.satoh-y-0323.yagura`
- macOS: `~/Library/Application Support/yagura`
- other Unix: `$XDG_CONFIG_HOME/yagura`, falling back to `~/.config/yagura`

A directory given explicitly (stage 1 or 2) is **exclusive**: it becomes the
only source of definitions and the bundled layer is switched off, so a name
it does not carry does not resolve at all. The config-root directory
(stage 3) is **overlaid** instead, name by name: a `claude-code.toml` placed
there is what `claude-code` resolves to, while every bundled name it does not
override stays available. Definitions are never merged with one another --
whichever file answers a name answers it whole.

A definition the directory layer *has* but cannot load -- unreadable,
unparsable, or carrying an invalid pattern -- is reported as the failure it
is, and is never quietly replaced by the bundled definition of the same name.
The bundled layer answers only a name the directory layer has no file for at
all.

There is no executable-relative lookup: no directory beside the daemon binary
or above it is searched, and no candidate path is inspected for where it
resolves to.

The GUI's new-window form offers the names from both layers as a dropdown,
with a leading entry that leaves the choice to the daemon's default. The list
is read once at startup and again every time the dropdown takes focus, so a
definition added to or removed from the directory layer while the application
runs shows up after focusing the dropdown again -- opening and closing it
without moving focus elsewhere in between does not re-read the directory. A
definition that is already selected stays selectable even after it
disappears, so a selection is never silently swapped for the default. When
the directory cannot be read at all, the previously read list is kept.

The MCP `list_detections` tool reports the same union, alongside
`bundled_names` and `directory_names` -- both always present, each possibly
empty -- so a caller can tell which layer a name came from. With an
explicitly configured directory, `bundled_names` is empty.

A window that ends up with no definition still opens: the daemon answers with
the window id and a warning describing why, which the GUI shows as a banner
and the MCP `open_window` tool returns as `detection_warning`. That covers a
definition that failed to load and a daemon with neither layer configured,
which says so in as many words (`no detection definition layer is configured;
status stays unknown`); the warning is absent only when a definition loaded,
so its presence is how a client tells a window running no detection from one
whose detection is working. That window stays at `unknown` (fail-safe), so it
can be closed but not interrupted.

```
yagura-daemon --detection claude-code --detection-dir path/to/detection
```

- `--detection <name>` selects the daemon's default `<name>.toml`, used by any
  window that does not request its own definition. The name must match
  `[a-z0-9_-]{1,64}` (it is used to build a file path, so separators, `..`,
  uppercase and empty values are rejected). Omitted, it falls back to the
  daemon's built-in default definition (`generic-shell`). The same name
  format and lookup rules apply to a per-window `detection` selection.
- `--detection-dir <path>` is stage 1 of the directory layer's resolution
  above, and is exclusive: that directory holds every definition the daemon
  will serve for that run. The path is used as written -- it is neither
  checked for existence nor canonicalized, so what the daemon reports and
  reads is exactly what was given.

A name no layer carries, a definition that fails to load, and neither layer
being configured at all each leave that window without detection: the daemon
keeps running, the window stays at `unknown` (fail-safe), and the window's own
`detection_warning` says which of the three it was. A malformed definition is
never partially applied: it is rejected as a whole and logged at error level.

## Schema

```toml
schema_version = 1             # must be 1; any other value is rejected
name = "example-cli"           # definition identity (data, not a type)
bottom_line_count = 5          # optional; height of the window (ending at
                               # the last line with a visible character)
                               # `region = "bottom"` rules see. Top level,
                               # before any table header. Default: 5

[debounce]                     # optional
confirm_ms = 300               # how long a raw judgement must hold before it
                               # becomes the confirmed status. Default: 300

[[rules]]                      # zero or more; all four keys are required
status = "waiting_approval"    # one of the seven statuses below
pattern = 'Do you want to proceed\?'
priority = 30                  # larger wins when several rules match
region = "bottom"              # "bottom" or "screen"; see What patterns
                               # are matched against

[quiescence]                   # optional; both keys required when present
status = "idle"
after_ms = 2000

[activity]                     # optional; both keys required when present
status = "running"
window_ms = 500

[composer]                     # optional; marker, separator and
                               # continuation_prefix required when present
marker = '\$\s*$'              # regex identifying the composer's marker
                               # (prompt) line
separator = " "                # string between the marker and the body;
                               # may be empty when marker's own match
                               # already reaches the body
continuation_prefix = ""       # string prefixing a wrapped continuation
                               # line; may be empty
blank_lines_in_body = false    # optional; a blank line above the cursor
                               # belongs to the body (default false)
left_panel = '^[^│]*│'         # optional, no default; regex matching a
                               # panel drawn left of the composer

[requests]                     # optional; every key optional
abandoned_at_turn_end = false  # this CLI drops a tool call still out when
                               # it ends its turn, without cancelling it.
                               # Default: false

[turn_end_key]                 # optional; both keys required when present
key = "ctrl_c"                 # "ctrl_c" or "escape": the key whose press
                               # ends the CLI's turn
shown = 'ctrl\+c interrupt'    # regex the bottom rows show while that key
                               # stops a turn
```

Statuses: `running`, `waiting_input`, `waiting_approval`, `interrupted`,
`idle`, `exited`, `unknown`.

Rejection is always loud: unknown keys -- at the top level and inside
`[debounce]`, `[quiescence]`, `[activity]`, `[composer]`,
`[requests]`, `[turn_end_key]` and each `[[rules]]` entry alike -- unknown status/region values, missing required keys, an
unsupported `schema_version`, an uncompilable `pattern`, more than 200 rules, or a source
file larger than 256 KiB all fail the load instead of being ignored.

The keys of the daemon's typing, which is gone, still load, each with one
warning that names it and says it has no effect: the whole `[input]` table
(`bracketed_paste`, `submit`, `ready_hold_ms`, `submit_delay_ms`, `clear`,
`delivery_confirm_ms`, `resend_max`, `reflect_wait_ms`, `probe`, `erase` and
`leading_keys`), and `placeholder` and `right_border` under `[composer]`.
Remove them from a definition of your own. `schema_version` stays 1.

### `[composer]`

`[composer]` says where this CLI draws its own input area: a definition that
carries this table declares a composer, and one that omits it does not. A
plain shell's screen alone cannot tell its prompt apart from a program's own
output (see the `generic-shell` paragraph above), so a definition for one
leaves this table out rather than guess. The composer is read only to keep
what is being typed there out of the rules (see the end of this section); the
daemon never writes into it.

`marker` is a regex identifying the composer's marker line (its prompt).
Match the marker itself, not what follows it, unless `separator` is empty --
then the marker's own match already reaches the body. `separator` is the
string between the marker and the body (commonly a single space); it may be
empty when `marker` covers that boundary itself. `continuation_prefix` is
what a display line wrapped from the body is prefixed with, for an editor
that draws one; leave it empty for one that does not.

The daemon finds the composer by its structure, walking up from the cursor:
a line starting with `continuation_prefix` is a continuation line and is
passed over, and the first line that is not one must match `marker`. The
marker line must therefore not itself start with `continuation_prefix`.
An editor that draws a blank line of a multi-line body as nothing, rather
than as `continuation_prefix`, needs `blank_lines_in_body = true` (it takes
effect only with a non-empty `continuation_prefix`): blank lines above a
shown cursor are then passed over too, while a cursor at the start of a blank
line is not taken to be in the composer. Declare it only for a CLI whose
dialogs hide the cursor: a dialog's layout has blank lines as well, and an
option row drawn with the marker's glyph could otherwise be taken for the
marker line.
When the walk meets another line before it reaches a marker line, the
composer is not observed, and so is a marker line with visible text after the
marker but no `separator`; nothing is taken out of a composer that cannot
be observed. A frame the CLI has erased and not yet drawn again reads this
way.

When every line from the cursor up to the top row of the screen is a
continuation line, the marker line has scrolled off the top (a body taller
than the screen). The daemon then reads the end of the body, from the top row
to the cursor, and only that: it is never taken for the whole body.

When the CLI draws another panel to the left of the composer, `left_panel` is
a regex matching that panel from the start of a line. Only when it matches
every line holding a visible character, and all its matches end at the same
column, does the daemon read the composer from that column rightwards: the
other keys are matched with that column as the start of the line, and a
cursor left of it is not in the composer. On any other screen the composer is
read from the first column, as without the key. The column is counted in
cells, a wide character taking two.

While the cursor is visible and the composer can be located this way, the
composer's body is taken out of the screen before `[[rules]]` are matched
(see [What patterns are matched against](#what-patterns-are-matched-against)),
so text being typed never decides the window's status.

### `[requests]`

`[requests]` says how this CLI treats a tool call of its own, made from its
window, that has not returned yet. It changes nothing about how the window
itself is detected.

| Key | Meaning |
| --- | --- |
| `abandoned_at_turn_end` | `true` when the CLI, on ending its turn, drops a call that is still out and does not cancel it, so the call's connection stays open and nothing tells the daemon the call was given up. The daemon then reads the window instead: once the window has been `running` since the call was made and comes to rest at `waiting_input`, `interrupted`, `idle` or `exited` (a window that is closed counts as `exited`), the call is answered at once (a call still waiting for an approver as `approval_timeout`, withdrawn with nothing done; a `wait_for_status` as timed out; a `wait_for_report` with no report taken, so a report arriving later stays in the inbox). `unknown` and `waiting_approval` never end a call. Leave it `false` for a CLI that cancels the calls it drops or keeps them alive past the end of a turn: for such a CLI, coming back to `waiting_input` does not mean the call was given up. Default: `false`. |

The bundled `codex-cli` definition sets it to `true`; `claude-code` and
`antigravity-cli` set it to `false`. `copilot-cli` leaves it out (so
`false`): Copilot CLI sends the server a cancellation for a call it drops,
whether the turn is interrupted or the call times out (checked with 1.0.101).

### `[turn_end_key]`

`[turn_end_key]` names the key whose press ends this CLI's turn, for a CLI
that runs no hook when a turn is interrupted. The daemon sees the press
itself: a key from an attached client (the window in the GUI), or the keys
`interrupt_window` writes for the profile's `[interrupt]`, when either is that
one key and nothing else.

| Key | Meaning |
| --- | --- |
| `key` | The key, by the word a profile's `[interrupt]` names it with: `ctrl_c` or `escape`. One key only. |
| `shown` | A regex matched against the `region = "bottom"` rows of every reading (the composer's body taken out): what the CLI shows while that key stops its turn. Compiled with the same limits as a rule's `pattern`. |

A press closes the window's turn only once the screen has shown `shown` at
least once since the last submission (a line sent with Enter, or a door that
moved the CLI on). The key that stops a turn changes between versions of a
CLI, and some CLIs change their version from inside the window (GitHub Copilot
CLI's `/update` and `/restart`, for example); every such command is a
submission, so the screen has to name the key again on the version now
running. A window whose screen has not shown it since the last submission is
not closed by the press, and its turn ends by reports and quiet as before.

After a press closed the turn, reports that would open a turn are dropped
until the next turn starts (see
[What a report means](#what-a-report-means)). A press of another key, the key
typed inside text or a paste, or several keys written together close nothing,
and a definition without the table is not affected by any press.

A daemon older than this table cannot load a definition that has it, so a
copy of such a definition placed in the `detection/` folder of the config
root leaves the windows that use it `unknown` under that daemon. The other
way round, an older copy placed there takes the place of the bundled
definition, table and all: a window that loads it does not end its turn on
the press. If you keep a copy of `copilot-cli.toml` there, add the bundled
`[turn_end_key]` table to it.

The bundled `copilot-cli` definition declares `ctrl_c`, shown as
`ctrl+c interrupt`. No other bundled definition has the table.

### What patterns are matched against

Output is fed to a virtual terminal, and rules are matched against the rendered
display text -- the same characters a human sees. Patterns can therefore be
written literally, without accounting for ANSI escape sequences, cursor motion
or repaints. `region = "bottom"` restricts matching to a window ending at the
last line holding a visible (non-whitespace) character and `bottom_line_count`
lines tall, which is where prompts, spinners and confirmation dialogs live. A
line painted with nothing but whitespace -- how a full-screen TUI often clears
and repaints unused space -- does not become the window's tail, but a blank
line still inside the window counts toward its height. A rule that matches a
whitespace-only line (for example `^\s*$`) therefore stops matching once that
line sits below the window's tail.

`region = "screen"` matches against every row of the screen, one row at a
time, for text whose distance from the bottom changes with the window's
height -- a question drawn in the middle of a full-screen dialog, for
instance. Since every row is read, anchor such a pattern to the whole row
(`^\s*...\s*$`) and give it a low `priority`, so the same text left in the
conversation history does not outrank the rules that read the CLI's current
state.

For a definition with a `[composer]`, the composer's body is taken out
first, while the cursor is visible and the composer can be located: the
marker line keeps only the marker and its `separator`, and the continuation
lines down to the cursor are dropped, before the bottom window is taken and
before a `region = "screen"` rule reads the rows. A
draft quoting a rule's text therefore does not match it, and a tall body
does not push the CLI's own lines out of the window. When the cursor is
hidden or the composer cannot be located -- a body taller than the screen,
whose marker line has scrolled out of view, for instance -- nothing is taken
out, and the window may read as `unknown` until the marker line is back in
view.

Matching starts when the daemon subscribes to the PTY, so a freshly started or
re-attached window begins at `unknown` and converges as new output arrives.

The daemon resolves its own `TERM` at startup (falling back to
`xterm-256color` if it inherited none, or an empty one) and declares that
value to every window's child process, unless a profile's `[env]` (or
`extra_env`) sets `TERM` itself, which wins instead. Since a TUI's glyph
choices can depend on what `TERM` it sees, write patterns that tolerate the
resulting rendering differences rather than assuming one fixed glyph set.

### Judgement order

1. The child process has exited -> `exited` (a fact; no pattern involved).
2. A `[[rules]]` entry (highest `priority` first) that names any status but
   `waiting_input` -> that status. A screen that shows a clear status always
   wins over a report.
3. A turn the CLI reported as open (see
   [Structured sources](#structured-sources---emit-status) below), over a
   screen that a rule reads as `waiting_input` or that no rule matches:
   `running` while the turn has had output in the last 5 seconds, or
   `waiting_approval` after an approval report where no rule matches at all.
   Never on a screen that was lost (`screen_lost`).
4. A `[[rules]]` entry that names `waiting_input`.
5. The time-based judgements, if the definition opted into them.
6. Otherwise `unknown` -- never an optimistic `idle` or `running` guess.

A window whose CLI sends no reports skips step 3, so it is judged by its
screen exactly as described here.

A raw judgement becomes the confirmed status only after holding for
`confirm_ms`, which absorbs spinner blinking and mid-repaint flicker. Two
transitions skip debouncing: `exited`, and the fallback to `unknown` after the
screen model is rebuilt.

A new judgement is made only on a screen the CLI has finished drawing. CLIs
hide the cursor while they repaint and show it again at the end, but a read
from the terminal can end halfway through, and the half-drawn screen can match
a rule the finished one does not. So the daemon judges when the cursor is
shown, or when it has stayed hidden and output has paused for 50 ms (a dialog
that keeps the cursor hidden). Output that keeps the cursor hidden without
pausing is still judged within 250 ms. These pauses only time the status.

Two fail-safe routes reach `unknown` without waiting out `confirm_ms`: a
screen model that failed and was rebuilt blank, and a window whose detection
stops reporting at all, which is reported as `unknown` rather than left frozen
at whatever it last showed. When a rebuilt screen is trusted again is
described under [Why a window is `unknown`](#why-a-window-is-unknown).

The bundled definitions share one convention for dialogs: a screen that waits
for the user to choose is matched on the key hint in its footer -- the part
whose shape holds still while the question above it varies -- and is reported
as `waiting_approval`. A dialog whose footer carries no such hint matches no
rule and falls to `unknown`, and `claude-code` opts into no time-based
judgement at all, so nothing there turns an unmatched screen into an `idle`
guess.

`codex-cli` reads a turn in progress from its footers: `Working (... esc to
interrupt)` while it thinks or runs a tool, and `tab to queue message` while
the composer holds a draft. While it streams an answer with an empty composer
it draws neither, and its screen is the same as when it waits for input, so
screen rules report `waiting_input` for that stretch; only a structured source
can tell the two apart, which is why the bundled Codex profiles give it hooks
(see [Bundled profiles](#bundled-profiles)).

For the same reason, `codex-cli` does not read a numbered option row (`› N. `)
as its composer: a selection list is drawn with that same glyph in column 0.
The cost lands on the other side -- while the user is typing a body that
starts with digits, a `.` and a space, the composer is not detected and the
window reports `unknown` until that text changes.

### Why a window is `unknown`

Every `unknown` the daemon confirms carries a reason for it. The reason
travels as `unknown_reason` on the wire (`list_windows`, `get_window_status`),
as a field of the same name on
the `window status confirmed` log line, and as a suffix beside the status in
the GUI. The six reasons, and what produces each:

| `unknown_reason` | Produced by |
| --- | --- |
| `no_definition` | A window that resolved no definition at all: a name no layer carries, a file the directory layer has but could not load, or a directory layer that could not be listed, so whether it carries the name was never established. It is the value such a window starts at and stays at -- no output moves it. |
| `not_yet_judged` | A window that did resolve a definition, before its first confirmed judgement. |
| `declared_unknown` | A definition that named `unknown` as a status itself: a `[[rules]]` entry whose `status` is `unknown`. A structured-source report of `unknown` does not produce it: such a report only withdraws the previous one. |
| `no_rule_matched` | A trusted screen that no rule matched and that the time-based judgements did not answer for either -- the `unknown` cells of the [two-clock table](#quiescence-and-activity) below. |
| `screen_lost` | A window whose screen model was rebuilt and has not been trusted again (see below). |
| `detector_stopped` | A window whose detection stopped reporting at all -- the fail-safe route named above, which overwrites whatever reason the window carried. |

A window loses its screen model only when the terminal engine behind it
panics: while taking output or while being read, which rebuilds the screen
blank at its size, or while being resized, which rebuilds it blank at the new
size. Nothing else drops what the screen holds: every byte of output reaches
the screen before anything else sees it, and a resize reflows the screen
rather than rebuilding it. A loss degrades the window to `unknown` at once,
without waiting out `confirm_ms`, and marks the screen untrusted. Trust comes back only when the window next
confirms a status that accepts input (`waiting_input` or `idle`); readable
output arriving is not on its own enough. While the screen is untrusted the
engine also declines to read a status out of `[activity]`'s clock, which a
rebuild leaves running: a definition carrying `[activity]` alone therefore
reports `screen_lost` until trust returns, rather than returning to its
activity status on elapsed time and undoing the degradation. `[quiescence]`'s
clock is not gated this way, so a definition carrying both sections still
settles on its quiescence status with no output at all.

Which reason a window carries says where to look. `no_rule_matched` is a
statement about what is on the screen: read the window itself and compare it
with the definition's patterns. The daemon also logs one `detector match
window` line per evaluation at `debug` level, carrying the exact lines the
rules were matched against -- the bottom window, and every row only when a
rule reads `region = "screen"` -- and which status matched, which is what tells a wrong pattern apart
from a rule looking at the wrong lines. `screen_lost`, `detector_stopped` and
`no_definition` are statements about the daemon's own reading rather than about
the screen, so the patterns are not the place to look for them at all.

### `[quiescence]` and `[activity]`

The two sections read two clocks, not one. `[quiescence]`'s `after_ms` always
measures `t`, the time since the last output chunk. `[activity]`'s `window_ms`
measures `t` as well -- except in a definition that has *both* sections, where
it measures `a` instead: the time since the last output chunk that was **not**
attributed to either of the two attribution sources, an input echo or a PTY
resize's own redraw (see [Echo and resize attribution](#echo-attribution)). An
interactive keystroke that submits nothing closes that band outright, and `a`
has no value at all until a chunk arrives that is not attributed to either
source.

They are opt-in because "no output" and "output flowing" mean different things
for different CLIs. Behavior depends on which sections are present, and applies
only while no rule matches (a matching rule always wins). A row without an
`[activity]` section has no `window_ms` of its own, so its first two bands
answer `unknown` wherever that boundary would have fallen:

| Present | `window_ms` measures | inside `window_ms` | past `window_ms`, `t` < `after_ms` | `t` >= `after_ms` |
| --- | --- | --- | --- | --- |
| both | `a` | `[activity].status` | previous judgement is held | `[quiescence].status` |
| quiescence only | `t` | `unknown` | `unknown` | `[quiescence].status` |
| activity only | `t` | `[activity].status` | `unknown` | `unknown` |
| neither | `t` | `unknown` | `unknown` | `unknown` |

Holding in the middle band is what lets a definition bridge activity to
quiescence without passing through `unknown`; it requires both sections, since
holding a stale judgement with only one of them would let a vanished
`waiting_approval` linger. Time-based transitions are debounced like any other,
so a confirmed `idle` arrives `after_ms + confirm_ms` after the last output.

`bottom_line_count = 5` and `confirm_ms = 300` are the daemon's defaults when a
definition omits them; `[quiescence]` / `[activity]` have no defaults at all.

#### Echo attribution

This section covers both attribution sources that keep `a` from advancing: an
input's echo and a PTY resize's own redraw.

Typing at a shell prompt is itself output: the terminal echoes every keystroke
back. Counting those echoes as activity would report a window as `running`
while someone is merely editing a line. What decides this is the payload, not
who wrote it: a write whose bytes carry no CR or LF submits nothing, whether
it came from a person typing or from a tool, so a
chunk arriving within `input_echo_window_ms` of such a write is attributed to
that write's echo and does not advance `a`. The chunk still reaches the
screen, so rules match it exactly as before; only the activity clock ignores
it.

Such a write also closes the activity band on the spot. A payload that
submits nothing is direct evidence that the window is not working on it yet,
and without closing the band a real output chunk from just before that write
would hold it open for the rest of its `window_ms`. The band reopens with the
first chunk that is not attributed to an echo.

A PTY resize is attributed the same way, through a second, independent clock:
a chunk that arrives within `resize_redraw_window_ms` of a resize being
requested is attributed to that resize's own redraw and does not advance `a`
either. Unlike a keystroke's echo, arming a resize never closes the activity
band on the spot -- a window already mid-work keeps its `running` status
across a resize, since a resize by itself is not evidence the window stopped
working. Re-requesting a resize before the previous window elapses re-arms it
with no upper bound on how often a live window may be resized in a row (a
drag is exactly that: a train of consecutive re-arms), and disarms the same
way a submitted line disarms echo attribution below.

Writes that may start work are never attributed: any write whose payload does
contain CR or LF -- a typed line, for instance -- and an interrupt, which carries no terminator
of its own but is never line editing, all leave the following output counting
as activity evidence -- and, the same way, immediately disarm any resize
window still open, so a submitted line's own output counts right away even if
it follows closely on a resize.

This is daemon behavior rather than part of a definition's schema -- there is
no TOML key for either window. `input_echo_window_ms` defaults to 150 ms and
is overridden with the `YAGURA_INPUT_ECHO_WINDOW_MS` environment variable;
`resize_redraw_window_ms` defaults to 150 ms as well and is overridden with
`YAGURA_RESIZE_REDRAW_WINDOW_MS`. For both, a missing value, one that fails to
parse, or `0` all fall back to that default. Neither override is clamped to an
upper bound: an operator-supplied value far beyond the default holds output
that arrives within it attributed for that long, which can leave a raw
judgement held in the middle band (never past `after_ms`, since that clock
always advances on every chunk) well past what a normal resize would produce.
That is an accepted trade-off for an escape hatch meant for unusual CLIs and
environments, not a bug either resolver guards against.

Attribution applies only to definitions carrying **both** `[activity]` and
`[quiescence]`. A definition with just one of them keeps measuring `window_ms`
on `t` and its timeline is untouched; neither echo nor resize attribution ever
suppresses anything there. Even where it does apply, the most attribution
can do is move a judgement out of the activity band and into the held middle
band: it never yields `idle` or `unknown`, because `after_ms` measures `t`,
which every output chunk advances whether it was attributed to either source
or not.

## Structured sources: `--emit-status`

A CLI that can run commands on its own lifecycle events can tell the daemon
where a turn starts and where it ends. The screen stays the basis of
detection: a report fills in what the screen cannot show -- a reply streaming
under an input line that looks idle, or an approval dialog too small to read
-- and never overrides a screen that shows a clear status (see
[Judgement order](#judgement-order)). A window whose CLI sends no reports is
judged by its screen alone.

```
yagura-daemon --emit-status <status> [--source <name>] [--skip-if <pointer>] [--for-instance <server>] [--credentials <path>]
```

- `<status>`: one of the seven statuses above; anything else is a command
  line it cannot parse (see below).
- `--source <name>`: who is reporting, matching `[a-z0-9_-]{1,64}`. Default:
  `cli`.
- `--skip-if <pointer>`: a JSON Pointer starting with `/`, for a CLI that
  runs the same hooks in its subagents' sessions and marks their input there.
  When the JSON the CLI writes to the hook's standard input has a value other
  than `null` at it, the hook is a subagent's and does only what a subagent's
  hook does: `running` and `waiting_approval` are still reported -- a
  subagent's work, a background one's included, is the window's work, so a
  background subagent that is working shows the window `running` -- and any
  other status is not sent, the command exiting 0 (see
  [Hook events](../../../profiles/README.md#hook-events)). The other doors
  take it the same way, and do nothing for a subagent's hook.
- `--for-instance <server>`: the name of the registration the hook was
  written for -- `yagura` for the default instance, `yagura-<instance>` for a
  named one. Given at most once, and taken by every hook door
  (`--emit-status`, `--deliver-context`, `--deliver-take`, `--deliver-wait`,
  `--report-session`, `--hook`) and by no other command.
- `--credentials <path>`: the file that names the window, for a window whose
  profile keeps its credentials out of the environment (see
  [Credentials](../../../profiles/README.md#credentials)). Given at most once,
  with the same doors and no other command. It is read only when the
  environment does not hold both `YAGURA_WINDOW_ID` and `YAGURA_STATUS_TOKEN`,
  each non-empty.

The command connects to the running daemon, sends one event and exits; it does
not start a daemon. Inside a window it exits non-zero if no daemon is
reachable. A command line it cannot parse there -- one written for a newer
daemon than the copy it starts, say -- is not refused: the command reads its
standard input to the end, sends nothing and exits 0, and logs one warning in
the instance's log naming the flag it could not read, never a value, so the
CLI does not report a failed hook. Outside one -- where
`YAGURA_STATUS_TOKEN` or `YAGURA_WINDOW_ID` is not set or is empty and no
`--credentials` file names a window, for example in a CLI session you started
in your own terminal, or one a tool of a window started -- it does not read its other
arguments at all: it reads its standard input to the end, sends nothing,
prints nothing and exits 0, so a hook that a CLI runs in every session stays
quiet in the sessions yagura did not open, even one written for a newer
daemon than the copy it starts. `--detection` and `--detection-dir` are
ignored here -- they configure a daemon this invocation is not starting.

A hook that registration writes into the CLI's own settings runs in every
session of that CLI, once for each instance registered there, and inside a
window each copy would reach the window's daemon. So inside a window a door
whose `--for-instance` names a registration other than the window's own
instance (read from `YAGURA_INSTANCE`) behaves as it does outside one: it
reads its standard input to the end, sends nothing, prints nothing and exits
0, before reading its other arguments. A `--for-instance` that is missing its
value, repeated or not a registration name is left to the command line, as
one it cannot parse.

For the event to authenticate and be attributed to the right window, the hook
must either run somewhere inside that window's child process tree, where it
inherits the `YAGURA_STATUS_TOKEN` and `YAGURA_WINDOW_ID` environment variables
the daemon injected when it spawned that window, or be handed the window's
credentials file with `--credentials`; `--emit-status` forwards the pair along
with the report (see [StatusEvent Authentication](../../../docs/mcp-setup.md#statusevent-authentication)).

```
yagura-daemon --hook <profile-id> <Event> [--for-instance <server>] [--credentials <path>]
```

`--hook` names a profile and one of its events instead of a status. What the
event does -- the status it reports, the session identifier it records, the
inbox it prints or hands over as a block, a stop it answers -- and how a
subagent's hook is told apart come from that profile's `[hook_events]` table
and `subagent_pointer`, which the daemon reads each time the hook runs (see
[Hook events](../../../profiles/README.md#hook-events)). A status reported
this way means what a `--emit-status` report means. `<profile-id>` follows the
profile id rule and `<Event>` matches `[A-Za-z0-9_-]{1,64}`. Its pointers
come from the profile, so it refuses `--skip-if`. In a window opened from a
launch profile that gives its CLI hooks, a `--hook` that names another
profile's id does nothing.

```
yagura-daemon --credentials <path> --print-answer <Event>
```

`--print-answer` prints the answer the window's open wrote for `<Event>`
beside its credentials file (see
[Answers a hook prints without the daemon](../../../profiles/README.md#answers-a-hook-prints-without-the-daemon)),
and nothing when there is none. It does not connect to the daemon, so it
answers while no daemon runs. A profile or event the daemon does not find
reports nothing, and the daemon logs a warning. A hook a registration writes
into a CLI's own settings uses this form, so its text stays the same from
release to release.

### What a report means

A report is a fact about the window's current turn, not a picture of its
screen. It has no expiry: it stands until the next report.

| Reported status | Effect |
| --- | --- |
| `running` | Opens a turn. |
| `waiting_approval` | Opens a turn that waits for the user's approval. |
| `waiting_input`, `idle`, `interrupted` | Closes the turn. |
| `unknown`, `exited` | Withdraws the previous report: the window is judged as if none had arrived. |

How an open turn combines with the screen:

- **A screen with a clear status wins.** A rule that reads `running`,
  `waiting_approval`, `interrupted`, `idle`, or a rule-declared `unknown`,
  decides the status whatever the last report said.
- **An open turn with recent output is `running`.** After a `running`
  report, a screen that a rule reads as `waiting_input`, or that no rule
  matches, is `running` as long as the window has produced output in the last
  5 seconds. Output that is only the echo of keystrokes, or a resize's own
  redraw, does not count (see [Echo attribution](#echo-attribution)), so
  typing into the input line does not keep a turn going.
- **A quiet turn closes.** When such a turn has had no output for 5 seconds,
  it closes and stays closed until the next `running` or `waiting_approval`
  report; output after that does not reopen it. A tool that runs for longer
  without printing closes the turn too, and the CLI's next report after the
  tool opens it again.
- **A turn without reports closes after 2 minutes.** A `running` turn that has
  had no report for 120 seconds closes even if output keeps coming, so a
  screen that redraws itself every few seconds cannot keep a turn open after
  an interrupt the CLI never reported. A CLI that uses tools reports each one,
  which keeps a working turn open; only an answer streamed for more than two
  minutes without any tool falls back to the screen.
- **Neither counts while the screen waits on you.** While a rule reads the
  screen as `waiting_approval`, or as an `unknown` the definition declares (a
  dialog it does not know, for example), neither the 5 seconds of quiet nor
  the 2 minutes without a report count: that time is yours, not the CLI's
  silence. Both resume where they stopped once the screen gives way, so a
  tool you allowed after a long look at its approval box still reads as
  `running`, and a screen that keeps showing and hiding an approval cannot
  hold a turn open beyond 2 minutes of the time in between.
- **An approval report stands in for a dialog the screen cannot show.** After
  a `waiting_approval` report, a screen that no rule matches is
  `waiting_approval`. A screen a rule reads as `waiting_input` is
  `waiting_input`. This turn does not close on silence, since a dialog waits
  without output; the next report ends it, or the screen does: a CLI need
  not report how an approval was answered (Claude Code's next report is the
  tool's end), so once a rule has read the dialog, a screen a rule reads as
  `running` in its place turns the approval into a running turn, and one it
  reads as `waiting_input`, `idle` or `interrupted` closes it.
- **A press of the turn-end key closes the turn and holds it closed.** For a
  definition with a [`[turn_end_key]`](#turn_end_key), a press of its key
  after the screen has shown it since the last submission closes the open
  turn at once, and also a turn a submission may have started that no
  report has opened yet. Until the next turn starts -- a submission, a door
  that moved the CLI on, a rule confirming `running` on the screen, or a
  report that closes the turn -- a `running` or `waiting_approval` report
  does not open a turn again. When a rule confirms `running`, the turn opens
  again for a window whose CLI has reported before; one that never reported
  stays judged by its screen. A press is not a report: it does not count for
  [When reports are missing](#when-reports-are-missing).
- **A closed turn leaves the screen to decide**, the same as no report at all.
- **A lost screen is not rescued.** While the screen model is untrusted (see
  [Why a window is `unknown`](#why-a-window-is-unknown)), the window stays at
  `screen_lost` whatever the reports say.

A report needs no finished screen to take effect, but the change it makes is
debounced by `confirm_ms` like any other.

### Bundled profiles

The bundled profiles for Claude Code, Codex CLI, Antigravity CLI, GitHub
Copilot CLI, Muse Code and Grok Build, and their WSL versions, give the CLI its hooks themselves, so nothing has to be written
by hand. Each event maps to a report as follows, checked with the CLI version
named:

| CLI | How the hooks reach it | `running` | `waiting_approval` | Closes the turn (`waiting_input`) |
| --- | --- | --- | --- | --- |
| Claude Code 2.1.285 (`claude-code`, `wsl-claude-code`) | `--settings` at every launch | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `PermissionRequest` | `Stop`, `StopFailure` |
| Codex CLI 0.159.2 on Windows, 0.158.0 in WSL (`codex-cli`, `wsl-codex-cli`) | `-c hooks.<Event>=...` at every launch, while their trust is registered | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `PermissionRequest` | `Stop`, `Interrupt` |
| Antigravity CLI 1.2.14 (`antigravity-cli`, `wsl-antigravity-cli`) | a `yagura` entry in `~/.gemini/config/hooks.json`, written by registration | `PreInvocation`, `PostToolUse` | -- | -- |
| GitHub Copilot CLI 1.0.101 (`copilot-cli`, `wsl-copilot-cli`) | a plugin directory, `--plugin-dir` at every launch | `userPromptSubmitted`, `preToolUse`, `postToolUse`, `postToolUseFailure` | -- | `agentStop` |
| Muse Code 1.4.12 (`muse-code`) | on Windows, a managed hooks file named in `%USERPROFILE%\.config\muse\settings.json`, written by registration; on macOS and Linux, `-c managed_hooks_path=...` at every launch | `UserPromptSubmit` | `PermissionRequest` | `Stop`, `StopFailure`, `Interrupt` |
| Muse Code 1.4.12 (`wsl-muse-code`) | a managed hooks file, `-c managed_hooks_path=...` at every launch | `UserPromptSubmit` | `PermissionRequest` | `Stop`, `StopFailure`, `Interrupt` |
| Grok Build 1.0.70 (`grok-build`, `wsl-grok-build`) | `~/.grok/hooks/yagura.json` (`yagura-<instance>.json` for a named instance), written by registration | `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | `Notification` with `permission_prompt` | `Stop`, `StopFailure`, `StopCancelled` |

How the hooks are placed, and what the profile tables look like, is in
[profiles/README.md](../../../profiles/README.md#hooks-injection). The same hooks
also run the commands a CLI reads its inbox through (see
[Delivery](../../../profiles/README.md#delivery)); those are not status
reports and are left out of the table.

**How a hook finds the daemon.** A hook's door connects to the daemon of its
window's instance; `--print-answer` alone connects to none (see above). When
the daemon starts, it places a copy of
its own executable (a hard link where the file system allows one) in a `bin`
folder under the instance's state directory -- on Windows,
`%LOCALAPPDATA%\io.github.satoh-y-0323.yagura\bin\yagura-daemon.exe` for the default instance;
on macOS, `~/.local/state/yagura/bin/yagura-daemon` (under `$XDG_STATE_HOME` when it is set)
rather than under `~/Library/Application Support`, whose space a hook command cannot carry -- and
puts that folder, which holds nothing else, in front of every window's
`PATH`. A hook can then run `yagura-daemon --emit-status ...` from whatever
shell its CLI uses. A profile's own `[env]` `PATH` replaces this one. Inside
WSL the hooks call `yagura-daemon.exe`, which a Linux shell finds through the
Windows part of WSL's `PATH`; a distribution with `appendWindowsPath = false`
in `/etc/wsl.conf` does not find it. When the folder cannot be prepared, the
daemon logs one warning at start-up and windows open with their `PATH`
unchanged.

**Claude Code** gets its hooks with every launch, added to whatever hooks you
configured yourself. The Windows profile writes them to a settings file under
the instance's state directory; the WSL profile passes the JSON on the command
line, since a Windows path is unreadable from inside WSL.

**Codex CLI** runs a hook passed with `-c` only when the hook is trusted, and
the key that records the trust contains characters a Windows npm shim cannot
pass on the command line. So registering yagura with Codex CLI from Settings >
Integrations also writes one `hooks.state` entry per hook, holding its trusted
hash, into `~/.codex/config.toml` -- the same place Codex records your own
"Trust all" -- and removing the registration takes them out. The daemon reads
that file each time a window opens and injects the hooks only while all the
entries are in place, so an unregistered Codex CLI gets no hooks and shows no
"Hooks need review" prompt. Each hook runs `yagura-daemon --hook codex-cli
<Event>` (`--hook wsl-codex-cli <Event>` in WSL), so its hash does not change
with a release; entries that are out of
date are written again when the window opens, while yagura is still
registered with Codex CLI (see
[Out-of-date hook entries](../../../docs/mcp-setup.md#out-of-date-hook-entries)).

**Antigravity CLI** reads hooks only from its own files, not from anything
passed at launch. Registering yagura with it from Settings > Integrations also
adds an entry named `yagura` (`yagura-<instance>` for a named instance) to
`~/.gemini/config/hooks.json`, which runs the daemon's copy in the `bin`
folder by its absolute path; removing the registration takes the entry out.
`agy` passes the command to a shell and escapes any quotes around a path, so
a path holding whitespace cannot be written: on Windows the entry names the
daemon by its 8.3 short name instead, and registration fails with a message
saying so when there is none (8.3 names can be turned off on a volume).
Every other entry in the file is left as it was. The entry applies to every
`agy` session on the machine, but outside a window the door does
nothing, and each command passes `--for-instance` so that another instance's
entry stays silent inside a window (see above). An `agy` you start by hand inside a yagura shell window
does report, for that window -- what it reports is true of that window.

**GitHub Copilot CLI** takes launch-time hooks only from a plugin directory,
so its profiles write one under the instance's state directory at every
launch and name it with `--plugin-dir`; the WSL profile hands it over as a
WSL path. Copilot loads the plugin's hooks only once you have answered its
folder trust prompt, so a window reports nothing before that.

**Muse Code** hands a hook only a fixed list of variables, plus those its
settings name in `managed_hooks_env_vars` for the one file `managed_hooks_path`
names, so yagura's hooks go in that managed file. On Windows, registering
yagura with Muse from Settings > Integrations writes the file and points
`%USERPROFILE%\.config\muse\settings.json` at it; it applies to every Muse
session, and each hook command first checks that it runs inside a window (see
[Hooks that run outside a window](../../../profiles/README.md#hooks-that-run-outside-a-window)).
On macOS and Linux, and in WSL, the profile passes its own file at every
launch instead and registers nothing. `PreToolUse` reports nothing, and
`PostToolUse` and `PostToolUseFailure` only answer an AI's interrupt (see
[Interrupt](../../../profiles/README.md#interrupt)): Muse runs them for its own
reviewing sub-sessions too, after the turn's `Stop`, and tells no subagent's
hook apart, so a status from them would leave the window `running`.

**Grok Build** reads hooks only from its own files, and its hooks file has
no place to keep one instance's entries apart from another's, so registering
yagura with it from Settings > Integrations writes a file of the
registration's own in `~/.grok/hooks`; removing the registration takes the
hooks out of it. Each hook runs the daemon's copy in the `bin` folder by its
absolute path, with its arguments given apart, so Grok Build starts it
without a shell -- the shell it would otherwise use differs from machine to
machine -- and inside WSL starts the Windows executable through its
`/mnt/...` path. Outside a window the daemon itself does nothing, and each
hook passes `--for-instance` so that another instance's file stays silent
inside a window (see above).
Grok Build runs the same hooks in a sub-agent's session, whose input carries
`subagentType`, so the profile names `subagent_pointer = "/subagentType"`: a
sub-agent reports only `running` and `waiting_approval` for the window, and
neither takes its inbox nor ends its turn. The approval
notification comes a few seconds after the tool's own `PreToolUse`, and the
approval box is read from the screen too. While any hook is in your own Grok Build settings, Grok Build asks about
folder trust in every folder outside your home directory (see
[Configuring Grok Build](../../../docs/mcp-setup.md#configuring-grok-build)),
and a window reports nothing before that prompt is answered.

### When reports are missing

A window expects reports when its hooks were actually given to the CLI: a
Claude Code or GitHub Copilot CLI window from a bundled profile, a Codex CLI
window whose trust entries were in place when it opened, an Antigravity
CLI, Muse Code (`muse-code`) or Grok Build window whose registration's entries
were in place when it opened, and a `wsl-muse-code` window. If such a window's screen shows a
running turn and 10 seconds later no report has arrived at all, the window is
marked:

- In the GUI, a yellow signal icon with a slash appears beside the plug icon,
  in the sidebar row and in the window's title bar, with the tooltip
  "Status reports from this CLI aren't arriving. yagura is reading the screen
  only."
- The daemon logs one warning: `the screen shows a running turn but no status
  report has arrived; the CLI's hooks may not be firing`, naming the
  definition.

The first report clears the mark for good. The mark never changes the
window's status, and the MCP tools do not carry it. It is the sign to look for
after a CLI update: a new version that renames its hook events, or changes
what Codex hashes, stops the reports, and the window falls back to its screen.
Only a window whose hooks never fire is caught this way; one that loses a
single event (only `Stop`, say) is not.

A window a restarted daemon took back is measured from the first running turn
after the one it was in has closed (output stopped for 5 seconds, or 2
minutes passed, either not counting while the screen waits on you), because
the new daemon cannot tell when that turn began.

### Limits

- **Claude Code sends no hook when you press Esc to interrupt a turn, or deny
  a permission.** The turn closes on 5 seconds of quiet instead, so the window
  returns to what its screen shows (`waiting_input` once its input line is
  back; the bundled definition has no `interrupted` rule) about 5 seconds
  plus `confirm_ms` later.
- **A window whose input line has grown taller than the screen** shows
  nothing a rule can match. While a turn is open it reads as `running`; once
  the turn closes it cannot be told apart from a selection list that shows no
  key hint, so it stays `unknown` (`no_rule_matched`).
- **Antigravity CLI reports neither an approval prompt nor an interrupt.**
  Only `PreInvocation` and `PostToolUse` are used: `agy` expects the output of
  a `PreToolUse` or `Stop` hook to carry a decision, which a status report does
  not give. Its approval prompts come from the screen alone, and its turns
  close on 5 seconds of quiet.
- **GitHub Copilot CLI reports neither an approval prompt nor an interrupt.**
  The hook Copilot runs for a permission request fires before its own rules
  decide whether to ask, so it would report prompts that never appear; the
  approval box is read from the screen alone. An interrupted turn runs no
  hook, so the bundled definition's `[turn_end_key]` closes the turn on the
  Ctrl+C press itself, typed in the window or written by its interrupt
  button, on a version whose footer shows `ctrl+c interrupt` (1.0.112). The
  window then reads as `waiting_input`, not `interrupted`, since the input
  line stays under the cancellation message. A turn cancelled with Esc twice
  is not closed by the press: the footer names Ctrl+C as the key that stops a
  turn (after a first Esc, 1.0.112 shows `esc again to interrupt` for about
  half a second and then `ctrl+c interrupt` again), and what Esc twice does
  depends on Copilot's state (its `/help` lists clearing the input,
  interrupting, stopping agents and rewinding). After Esc twice the window
  stays `running` until 5 seconds of quiet close the turn, and longer while
  Copilot keeps redrawing its screen. Version 1.0.101 shows
  `esc esc interrupt` and never `ctrl+c interrupt`, so there no press closes
  the turn, Esc twice included, and it closes on quiet in the same way.
- **GitHub Copilot CLI's session sidebar can take the keys.** While the
  sidebar holds the focus, and on the tabs other than Current, keys act on a
  list rather than reaching the input line, so the window reads as `unknown`
  (`declared_unknown`).
- **Muse Code puts a submitted text back in its input line when the turn is
  interrupted before any output.** Pressing Esc between a submit and the
  first output cancels the turn and refills the composer with what was sent;
  send it again or clear it by hand.
- **Grok Build's folder trust prompt is read from its question.** The
  question sits in the middle of the screen, at a distance from the bottom
  that changes with the window's height, so the `grok-build` definition reads
  it with a `region = "screen"` rule, as `waiting_approval`, only when no
  other rule matches. Two screens read otherwise: when a popup covers the
  bottom of the screen and hides the input box, the same question left alone
  on a row of the history reads as `waiting_approval`; and in a window too
  narrow for the question to fit on one row, the prompt reads as `unknown`.
- **After a cancelled turn, Grok Build's history holds the focus.** Once a
  turn is cancelled with Ctrl+C, or Esc is pressed while a permission box is
  shown, keys act on the history rather than the input line, and its footer
  shows `Space:prompt`. The window reads as `unknown` (`declared_unknown`), so
  nothing is pushed to it and `interrupt_window` is refused, until the focus
  is back in the input line on the window's screen.
- **Grok Build's interrupt key also empties a draft.** The user's own
  interrupt writes Ctrl+C, which cancels a running turn, but which, while the
  window waits for input, erases whatever its input line holds, text typed by
  hand included. An AI's interrupt writes no key to Grok Build (see
  [Interrupt](../../../profiles/README.md#interrupt)).

### Writing hooks by hand

A window opened from a bundled profile needs none of this. For a CLI started
inside a window from a raw command line, or from a profile of your own without
a `[hooks]` table, the CLI's own settings can run the same commands. Such a
window does not expect reports, so it is never marked when they are missing.
These commands find the window through its environment, which a window opened
from a profile with the `launch` credentials mode does not have; they report
nothing there.

**Claude Code**, in `.claude/settings.json` (or `~/.claude/settings.json`),
the same hooks the bundled profiles pass:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status running --source claude-code" }] }
    ],
    "PreToolUse": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status running --source claude-code" }] }
    ],
    "PostToolUse": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status running --source claude-code" }] }
    ],
    "PermissionRequest": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status waiting_approval --source claude-code" }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status waiting_input --source claude-code" }] }
    ],
    "StopFailure": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status waiting_input --source claude-code" }] }
    ]
  }
}
```

The event names are those of the Claude Code hooks reference
(<https://code.claude.com/docs/en/hooks.md>). Claude Code runs each hook's
`command` as a child of the Claude Code process yagura spawned, so the hook
inherits that window's `YAGURA_STATUS_TOKEN` and `YAGURA_WINDOW_ID`, and finds
`yagura-daemon` through the window's `PATH`.

**Codex CLI**, in `~/.codex/hooks.json`, the same events as the bundled
profiles:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status running --source codex-cli" }] }
    ],
    "PreToolUse": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status running --source codex-cli" }] }
    ],
    "PostToolUse": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status running --source codex-cli" }] }
    ],
    "PermissionRequest": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status waiting_approval --source codex-cli" }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status waiting_input --source codex-cli" }] }
    ],
    "Interrupt": [
      { "hooks": [{ "type": "command", "command": "yagura-daemon --emit-status waiting_input --source codex-cli" }] }
    ]
  }
}
```

Points worth knowing about this placement, and about Windows:

- `~/.codex/hooks.json` is user-level: it applies to every Codex session on
  the machine, including sessions yagura never spawned. In those,
  `--emit-status` does nothing -- provided `yagura-daemon` can be found there
  at all. Outside a window the `bin` folder is not on `PATH`, so unless the
  daemon is on your own `PATH`, Codex reports the hook as failed; nothing else
  changes. The Codex hooks reference also lists `<repo>/.codex/hooks.json`,
  read for a repository you have trusted; **that placement is not verified in
  this document**. The `hooks.json` placement was verified with Codex 0.152.1
  on Windows.
- On Windows the command line is run through `cmd`, which strips the leading
  and trailing quote of a line that begins with `"`. A quoted executable path
  therefore fails with `hook exited with code 1`. Write the path unquoted,
  which in turn means keeping the daemon at a path containing no spaces.
- Hooks are trusted explicitly. A new or edited `hooks.json` makes Codex open
  a "Hooks need review" prompt at startup, offering to review them or to trust
  them and continue, and `/hooks` reaches the same place from inside a
  session. Trust them there rather than starting Codex with
  `--dangerously-bypass-hook-trust`.
- `--source` is a label naming who is reporting, not an authenticated
  identity: authentication is per window and carried by the token alone, with
  the limits described in
  [Reported window status is not fully authenticated](../../../docs/mcp-setup.md#reported-window-status-is-not-fully-authenticated).
