# Detection definitions

This directory holds the state-detection definitions the yagura daemon matches
PTY output against: one TOML file per CLI, named `<name>.toml`.

A definition holds both directions of that terminal: how to *read* it -- the
patterns and clocks below -- and, in its `[composer]` and `[input]` tables,
how to *write* to it. `[composer]` says whether the daemon may write into a
window running this definition at all, and where on screen to look before it
does; `[input]` says how, once it may. A definition that leaves `[composer]`
out declares no composer, so the daemon never puts a line of its own
composition in front of that CLI (see [`[composer]`](#composer)); one that
declares a composer but names no `[input].probe` gets no such line either.

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
- `generic-shell` -- a plain interactive shell, and the daemon's built-in
  default when nothing selects a definition. It declares no `[composer]`: a
  plain shell's screen alone cannot tell its prompt apart from a program's
  own output, so it only reports the window's status and the daemon never
  writes into a window running it.

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
full rule.

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
region = "bottom"              # only "bottom" is accepted today

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

[input]                        # optional; every key optional
bracketed_paste = false        # wrap the body in ESC[200~ / ESC[201~ before
                               # writing it. Default: false
submit = "\r"                  # the bytes that submit a line, written
                               # verbatim. 1..=8 bytes. Default: "\r"
ready_hold_ms = 0              # how long the window's status must have
                               # stayed one that accepts input, once, before
                               # the daemon writes into it. 0..=10000.
                               # Default: 0
submit_delay_ms = 800          # how long to wait after the body before the
                               # submit bytes. 0..=5000. Default: 800
clear = ""                     # one key that takes text back out of the
                               # composer and does nothing on an empty one;
                               # the daemon repeats it to retract a write it
                               # could not confirm. 0..=8 bytes; empty means
                               # "send nothing". Default: ""
probe = "x"                    # one printable, non-blank character other
                               # than "[", typed to learn that everything
                               # before it was handled: behind every line
                               # the daemon writes, before writing into a
                               # composer that reads empty, and after a
                               # retraction. Omitted, the daemon writes no
                               # line of its own into the window.
                               # Default: none
erase = "\u007f"               # the key that erases one character, used to
                               # take the probe back out. 1..=8 bytes.
                               # Default: DEL
delivery_confirm_ms = 8000     # how long to wait, after the submit bytes,
                               # for the composer to empty or the status to
                               # react. 0..=30000; 0 means "do not look for
                               # either". Default: 8000
resend_max = 0                 # how many times an unconfirmed submit may be
                               # written again. 0..=4. Default: 0

[requests]                     # optional; every key optional
abandoned_at_turn_end = false  # this CLI drops a tool call still out when
                               # it ends its turn, without cancelling it.
                               # Default: false
```

Statuses: `running`, `waiting_input`, `waiting_approval`, `interrupted`,
`idle`, `exited`, `unknown`.

Rejection is always loud: unknown keys -- at the top level and inside
`[debounce]`, `[quiescence]`, `[activity]`, `[composer]`, `[input]`,
`[requests]` and each `[[rules]]` entry alike -- unknown status/region values, missing required keys, an
unsupported `schema_version`, an uncompilable `pattern`, an `[input]` value
outside its range or contradicting another, more than 200 rules, or a source
file larger than 256 KiB all fail the load instead of being ignored.

### `[composer]`

`[composer]` says where this CLI draws its own input area, and is what makes
a window running this definition writable at all: a definition that carries
this table declares a composer, and one that omits it does not. The
distinction is not cosmetic -- a plain shell's screen alone cannot tell its
prompt apart from a program's own output (see the `generic-shell` paragraph
above), so a definition for one leaves this table out rather than guess.

A window whose definition declares no composer is never written to by the
daemon, through any path: `send_message`, a `send_handoff` envelope, a task
instruction's notice and a chat delivery are all refused there as
`no_composer` before anything is read, and the window is never a chat
participant either, regardless of `[chat].exclude_detections`. `send_keys` is
unaffected, since it writes a caller's own bytes rather than anything the
daemon composed.

A definition that declares a composer but names no `[input].probe` is
written to only as far as needs no judgement. Whether a line the daemon
writes has shown in the composer is decided by the probe typed behind it
(see [`[input]`](#input)), so without one the daemon writes no such line: a
one-line `send_message` (text ending in its single terminator), a
`send_handoff` envelope and a task instruction's notice are refused there as
`no_composer` the same way, no notice or chat intro is typed there, and the
window is not a chat participant. A `send_message` text with no terminator, or one
that is only a terminator, is still written as it is, behind the same screen
check every write goes through. The definition still loads and the window's
status is still detected; naming a `probe` is all it takes for the daemon to
write lines into it.

The daemon writes only on a screen the CLI has finished painting: the cursor
is shown and no synchronized update (`CSI ? 2026 h`) is open. A screen that
is not finished -- one read halfway through a repaint, or one held open in a
synchronized update -- is neither written to nor declined on; the daemon reads
it again once more output arrives, however long that takes, rather than
taking it as finished after a pause. A screen the rules read as an approval
prompt or declare `unknown` is declined on whether or not it is finished,
since a dialog keeps the cursor hidden. So a CLI that hides the cursor while
its composer sits idle receives no write from the daemon at all: that is the
safe side, since such a screen cannot be told apart from a half-drawn dialog.

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
marker but no `separator`; the daemon does not write into a composer it
cannot observe, and waits for a reading it can use rather than deciding on
one it cannot. A frame the CLI has erased and not yet drawn again reads this
way.

When every line from the cursor up to the top row of the screen is a
continuation line, the marker line has scrolled off the top (a body taller
than the screen). The daemon then reads the end of the body, from the top row
to the cursor, and only that: it is never taken for the whole body.

While the cursor is visible and the composer can be located this way, the
composer's body is taken out of the screen before `[[rules]]` are matched
(see [What patterns are matched against](#what-patterns-are-matched-against)),
so text being typed never decides the window's status.

### `[input]`

`[input]` is read when the daemon writes into a window running this
definition -- delivering a [`send_handoff`](../../../docs/mcp-setup.md#send_handoff)
envelope, for instance. The body and the submit bytes are written as two
steps, `submit_delay_ms` apart, because a CLI that reads a fast burst of input
as a paste takes a carriage return arriving inside that burst as a line break
rather than as "submit". After the submit bytes are written, the daemon
watches the window for `delivery_confirm_ms` to see whether the receiver
took the envelope: its painted composer empties with nothing else written into
the window, or its status moves from waiting for input to working. When
neither shows, the daemon may press the submit key
again, up to `resend_max` times. A re-send only ever presses that key: it
runs where the envelope this delivery wrote is still the only thing in the
composer, so the body is neither cleared nor written a second time, and a
definition needs no `clear` sequence to declare `resend_max`. The key is
pressed on its own only when the screen shows that envelope itself. When the
screen shows it as a placeholder, or shows only its end, the daemon first
types the probe behind it, and presses the key once the probe has shown at the
cursor and been erased again.

Whether a written body has shown is decided by the order the CLI handles its
input in, never by a clock. The daemon types `probe` right behind the body and
reads the composer again whenever the window draws output, changes status or
takes another write:

- The composer ends with the probe behind something: the CLI has handled the
  body. The daemon types `erase`, and writes the submit bytes once the
  composer reads as it did before the probe. A body taller than the screen
  shows only its end: that end, followed by the probe, counts when it ends the
  body once the probe is left off, and the composer must read as such an end
  again before the submit bytes are written. A composer that cannot be read
  at all is waited on, not taken for either.
- The probe shows alone: the CLI dropped the body, as a CLI that is still
  starting up can. The daemon erases the probe and reports the write as
  `not_written`, with nothing left behind.
- Nothing shows yet: the CLI has not got to its input. The daemon keeps
  waiting, for as long as that takes. A screen that stops accepting input
  meanwhile -- an approval dialog -- is waited out the same way rather than
  refused. Only another writer's input into the window (typing, `send_keys`,
  `interrupt_window`), the child exiting (`close_window` included), or the
  requester cancelling the call or disconnecting ends the wait without an
  answer from the screen.

So a CLI that stops handling its input -- a dialog this definition has no rule
for, for instance -- holds a write into its window until one of those events
comes; see [`send_handoff`'s Maximum wait](../../../docs/mcp-setup.md#send_handoff)
for how to end one. This is also why the daemon writes no line of its own
into a window whose definition names no `probe` (see
[`[composer]`](#composer)): nothing would say whether the line showed.

`reflect_wait_ms`, which earlier definitions set to bound how long the daemon
read the composer for a write to show, is still accepted in `[input]` so that
such a definition keeps loading, with any value. It has no effect.

`clear` is used to take the daemon's own text back out of the composer. Its
main use is a requester that leaves -- an MCP client cancels the call, or its
connection closes -- after the body was written and before the submit bytes:
the daemon then takes the body back rather than leave it in front of the
receiver, and finishes that take-back even though nobody is waiting for the
answer any more, holding the window's turn meanwhile so nothing is written
on top. `clear` is one key that the daemon may repeat until the composer is
empty: it must take text back out of the composer a piece at a time -- a
display line, for instance -- and do nothing on an empty one. The take-back is
`clear` repeated once per display line the body may occupy at the window's
current width, with a margin, written in a single write right behind the
body; since the CLI handles its input in order, the keys follow the body
however late the CLI gets to it, and the keys left over land on an empty
composer. The bundled definitions all use Ctrl+U (`"\u0015"`).

`clear` also takes the daemon's text back when another writer's input reaches
the window while that text sits unsubmitted in the composer. The take-back is
written in the same write, in front of the other writer's bytes, counted at the
window's width at that moment; it relies on `clear` doing nothing on an empty
composer. The daemon then writes nothing more in that attempt -- no submit
bytes, no re-send. It reads the composer once the screen accepts input and
writes `probe` only if its own text is still all that shows; the probe then
tells whether the take-back got through (it is taken back again if not), and is
erased. When the composer shows anything else, nothing is written and the text
is reported as possibly left behind. The one write nothing is put in front of
is the interrupt key sequence (`interrupt_window`'s bytes): it reaches the CLI
as sent, and the daemon takes its text back after it.

An empty composer right after the take-back proves nothing: a CLI that has
not read the body yet shows exactly that. So the daemon types `probe` behind
the take-back and reads the composer until it shows the probe and nothing
else. The CLI handles its input in order, so at that point the body and the
take-back have both been handled and nothing of the body is left; the daemon
then types `erase` and reports the retraction as leaving nothing behind once
the composer reads empty. Everything else is reported as leaving something
behind, and a window a handoff opened for it stays open:

- The probe shows behind something -- the take-back left part of the body.
  The daemon types `erase` (input is handled in order, so it removes the
  daemon's own probe whenever the CLI gets to it).
- Anyone else writes into the window meanwhile. The daemon stops there: it
  writes nothing more to finish the delivery, and beyond taking its own text
  back (in front of that writer's input, as described above) and erasing its
  own probe it takes back or erases nothing.

Until the probe shows, the daemon waits for it, the same way it waits for a
body to show.

The submit bytes never follow a take-back, even when the body shows up in
the composer afterwards: the keys queued behind it may still empty or cut it
short.

The same probe guards the first write. A composer that reads empty may only
not have drawn what someone just typed or pasted into it, and a body written
there would land behind that draft and be submitted with it. So, unless the
only writes since the delivery's own last attempt are its own, the daemon
types `probe` before writing the body and reads the composer until the probe
shows. The probe alone means the composer is empty: the daemon types `erase`
and writes the body once the composer reads empty again. Anything in front of
the probe is someone's draft: the daemon erases the probe and leaves the
draft alone. A delivery about to re-send the submit key over its own envelope
does the same when someone else wrote into the window since its last attempt,
and presses the key only when the probe shows right behind the envelope. A
requester that leaves before such a probe shows leaves it to the next write
into the window, which erases it once it shows alone with nothing written
since.

#### Choosing `probe` for a CLI

`probe` is a character the daemon types into the CLI's composer, so it has
to be harmless there. Check a candidate on the CLI itself, with an empty
composer, before putting it in a definition:

1. Typed alone, it shows in the composer as that one character, and typed
   behind text already there, it shows at the end of it.
2. It triggers nothing: no command menu, completion list, mode switch or
   shortcut. Characters that start commands or mentions at the beginning of
   a line (`/`, `@`, `!`, `#`, `?`) are poor choices; `~` disconnected Claude
   Code from its editor integration.
3. `erase` (DEL unless the definition says otherwise) removes it and leaves
   the composer empty.

`[` is never accepted as `probe`, and a definition naming it fails to load:
every line the daemon writes on its own -- a `[yagura handoff] ` envelope,
and the chat, task, plan and report lines -- starts with `[`, so such a probe
showing alone could not be told apart from one of those lines beginning to
show.

The bundled definitions use `x`, checked as above on each of their CLIs and
on PowerShell 5.1. If a CLI -- or a later version of one -- treats `x`
differently, change `probe` in that definition to another character that
passes the same three checks; nothing else in the daemon depends on which
character it is. The probe only means "everything before me was handled"
while the composer takes typed text: in a dialog, or in an editor mode where
keys are commands (vim's normal mode, for instance), it is read as
something else, never shows where the daemon looks for it, and the write
waits as described above.

An empty `clear` cannot retract anything -- the body the daemon wrote for a
requester that left, or in front of which another writer typed, is left
sitting in the composer instead, and the
delivery ends as `not_written` with the envelope still in front of the
receiver (see
[`send_handoff`'s delivery outcomes](../../../docs/mcp-setup.md#send_handoff)).

`ready_hold_ms` decides *when* the daemon may start writing. A CLI that is
still starting up can alternate between drawing its prompt and drawing a
working line, so a prompt on screen is not on its own evidence that input
will be accepted; this key asks for the prompt to stay. The judgement is made
once per window: once a window has been seen to hold a status that accepts
input for that long, nothing written to it afterwards waits for it again. The
wait applies only while a window has not yet been seen to hold, and only when
the write is requested at a moment that window is in one of those statuses --
a write into a busy window is never delayed, and a write into an existing
window never fails because of this key, though a window a handoff's `profile`
opened and that never holds does end as `window_not_ready`. A definition that
leaves this key out may see a delivery into a freshly started window reported
as `unconfirmed`, so a new vendor definition should measure it: sample the
confirmed status every 50 ms over the first ten seconds of a window's life,
read the longest run of a status that accepts input within the startup phase,
and take that sample under load as well -- with other windows working, an
observed run grew more than twofold. Two limits on what the value means: it
is measured from the last confirmed *change*, so a move from one status that
accepts input to another restarts it, and it is never compared against
`[quiescence].after_ms`, which the engine reads only when no rule matches at
all and which therefore does not bound how long a rule-driven judgement
lasts.

Two combinations are contradictory and are rejected rather than quietly
resolved:

- `delivery_confirm_ms = 0` with `resend_max > 0` -- a re-send only ever
  happens as the outcome of a confirmation that was looked for and did not
  come, so switching the confirmation off leaves `resend_max` meaning
  nothing.
- `delivery_confirm_ms > 0` with `submit_delay_ms` at or below the same
  definition's `[debounce] confirm_ms` -- the reaction that confirms a
  delivery is read after the submit bytes, and it can only be told apart from
  the body's own echo if that echo has had time to become the confirmed
  status first. A definition that omits `submit_delay_ms` is judged on the
  800 ms default it will actually run under. This check has one blind spot: a
  definition that writes no `[debounce]` table of its own runs on the
  daemon's `confirm_ms` instead, which the loader never sees, so no such
  definition is checked at all.

### `[requests]`

`[requests]` says how this CLI treats a tool call of its own -- a
`send_message` or `send_handoff` made from its window -- that has not
returned yet. It changes nothing about how the window itself is detected or
written to.

| Key | Meaning |
| --- | --- |
| `abandoned_at_turn_end` | `true` when the CLI, on ending its turn, drops a call that is still out and does not cancel it, so the call's connection stays open and nothing tells the daemon the call was given up. The daemon then reads the window instead: once the window has been `running` since the call was made and comes to rest at `waiting_input`, `interrupted`, `idle` or `exited` (a window that is closed counts as `exited`), the call is answered `not_written` at once and its text is taken back, as for a cancelled call. `unknown` and `waiting_approval` never end a call. Leave it `false` for a CLI that cancels the calls it drops or keeps them alive past the end of a turn: for such a CLI, coming back to `waiting_input` does not mean the call was given up. Default: `false`. |

The bundled `codex-cli` definition sets it to `true`; `claude-code` and
`antigravity-cli` set it to `false`.

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

For a definition with a `[composer]`, the composer's body is taken out
first, while the cursor is visible and the composer can be located: the
marker line keeps only the marker and its `separator`, and the continuation
lines down to the cursor are dropped, before the bottom window is taken. A
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
pausing is still judged within 250 ms. These pauses only time the status; a
write never rests on them (see [`[composer]`](#composer)).

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
travels as `unknown_reason` on the wire (`list_windows`, `get_window_status`,
and a chat delivery that was never attempted), as a field of the same name on
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
rules were matched against -- the window `region` selected, not the whole
screen -- and which status matched, which is what tells a wrong pattern apart
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
it came from a person typing, from a tool, or from a handoff's body, so a
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
contain CR or LF -- a typed line, a `send_message` or `send_text` carrying
one, a handoff's submit bytes -- and an interrupt, which carries no terminator
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
yagura-daemon --emit-status <status> [--source <name>]
```

- `<status>`: one of the seven statuses above; anything else exits non-zero.
- `--source <name>`: who is reporting, matching `[a-z0-9_-]{1,64}`. Default:
  `cli`.

The command connects to the running daemon, sends one event and exits; it does
not start a daemon. Inside a window it exits non-zero if no daemon is
reachable. Outside one -- where `YAGURA_STATUS_TOKEN` or `YAGURA_WINDOW_ID` is
not set, for example in a CLI session you started in your own terminal -- it
sends nothing, prints nothing and exits 0, so a hook that a CLI runs in every
session stays quiet in the sessions yagura did not open. A command line it
cannot parse is refused either way. `--detection` and `--detection-dir` are
ignored here -- they configure a daemon this invocation is not starting.

For the event to authenticate and be attributed to the right window, the hook
must run somewhere inside that window's child process tree: it inherits the
`YAGURA_STATUS_TOKEN` and `YAGURA_WINDOW_ID` environment variables the daemon
injected when it spawned that window, and `--emit-status` forwards both along
with the report (see [StatusEvent Authentication](../../../docs/mcp-setup.md#statusevent-authentication)).

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
- **An approval report stands in for a dialog the screen cannot show.** After
  a `waiting_approval` report, a screen that no rule matches is
  `waiting_approval`. A screen a rule reads as `waiting_input` is
  `waiting_input`. This turn does not close on silence, since a dialog waits
  without output; the next report ends it, or the screen does: a CLI need
  not report how an approval was answered (Claude Code's next report is the
  tool's end), so once a rule has read the dialog, a screen a rule reads as
  `running` in its place turns the approval into a running turn, and one it
  reads as `waiting_input`, `idle` or `interrupted` closes it.
- **A closed turn leaves the screen to decide**, the same as no report at all.
- **A lost screen is not rescued.** While the screen model is untrusted (see
  [Why a window is `unknown`](#why-a-window-is-unknown)), the window stays at
  `screen_lost` whatever the reports say.

A report needs no finished screen to take effect, but the change it makes is
debounced by `confirm_ms` like any other.

### Bundled profiles

The bundled profiles for Claude Code, Codex CLI and Antigravity CLI, and their
WSL versions, give the CLI its hooks themselves, so nothing has to be written
by hand. Each event maps to a report as follows, checked with the CLI version
named:

| CLI | How the hooks reach it | `running` | `waiting_approval` | Closes the turn (`waiting_input`) |
| --- | --- | --- | --- | --- |
| Claude Code 2.1.285 (`claude-code`, `wsl-claude-code`) | `--settings` at every launch | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `PermissionRequest` | `Stop`, `StopFailure` |
| Codex CLI 0.159.2 on Windows, 0.158.0 in WSL (`codex-cli`, `wsl-codex-cli`) | `-c hooks.<Event>=...` at every launch, while their trust is registered | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `PermissionRequest` | `Stop`, `Interrupt` |
| Antigravity CLI 1.2.14 (`antigravity-cli`, `wsl-antigravity-cli`) | a `yagura` entry in `~/.gemini/config/hooks.json`, written by registration | `PreInvocation`, `PostToolUse` | -- | -- |

How the hooks are placed, and what the profile tables look like, is in
[profiles/README.md](../../../profiles/README.md#hooks-injection).

**How a hook finds the daemon.** When the daemon starts, it places a copy of
its own executable (a hard link where the file system allows one) in a `bin`
folder under the instance's state directory -- on Windows,
`%LOCALAPPDATA%\io.github.satoh-y-0323.yagura\bin\yagura-daemon.exe` for the default instance -- and
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
"Hooks need review" prompt.

**Antigravity CLI** reads hooks only from its own files, not from anything
passed at launch. Registering yagura with it from Settings > Integrations also
adds an entry named `yagura` (`yagura-<instance>` for a named instance) to
`~/.gemini/config/hooks.json`, which runs the daemon's copy in the `bin`
folder by its absolute path; removing the registration takes the entry out.
Every other entry in the file is left as it was. The entry applies to every
`agy` session on the machine, but outside a window `--emit-status` does
nothing (see above). An `agy` you start by hand inside a yagura shell window
does report, for that window -- what it reports is true of that window.

### When reports are missing

A window expects reports when its hooks were actually given to the CLI: a
Claude Code window from a bundled profile, a Codex CLI window whose trust
entries were in place when it opened, and an Antigravity CLI window whose
hooks entry was in place when it opened. If such a window's screen shows a
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

### Limits

- **Claude Code sends no hook when you press Esc to interrupt a turn, or deny
  a permission.** The turn closes on 5 seconds of quiet instead, so the window
  returns to what its screen shows (`interrupted`, for example) about 5
  seconds plus `confirm_ms` later.
- **A window whose input line has grown taller than the screen** shows
  nothing a rule can match. While a turn is open it reads as `running`; once
  the turn closes it cannot be told apart from a selection list that shows no
  key hint, so it stays `unknown` (`no_rule_matched`). With a `continuation_prefix` declared, the
  daemon can still write into it: it reads the end of the input line, which is
  what a write is confirmed against.
- **Antigravity CLI reports neither an approval prompt nor an interrupt.**
  Only `PreInvocation` and `PostToolUse` are used: `agy` expects the output of
  a `PreToolUse` or `Stop` hook to carry a decision, which a status report does
  not give. Its approval prompts come from the screen alone, and its turns
  close on 5 seconds of quiet.

### Writing hooks by hand

A window opened from a bundled profile needs none of this. For a CLI started
inside a window from a raw command line, or from a profile of your own without
a `[hooks]` table, the CLI's own settings can run the same commands. Such a
window does not expect reports, so it is never marked when they are missing.

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
