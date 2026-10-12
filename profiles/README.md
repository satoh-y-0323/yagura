# Launch profiles

A launch profile is a saved way to open a child window: what to run, which
detection definition to match its output against, and where to start it. Each
profile is one TOML file, named `<id>.toml`. The files next to this document
are the profiles shipped with the application; your changes to them, and
profiles of your own, live in the daemon's profiles directory (see
[Where profiles live](#where-profiles-live)).

## Where profiles live

The profiles in this directory are built into the daemon and are the base
layer. Your own changes live in a separate directory, resolved in three
stages, in order:

1. `--profiles-dir <path>` (command-line flag)
2. the `YAGURA_PROFILES_DIR` environment variable (an empty value counts as
   unset)
3. `<config root>/profiles`

The config root is the same one `playbook.toml` lives in:

- Windows: `%LOCALAPPDATA%\io.github.satoh-y-0323.yagura`
- macOS: `~/Library/Application Support/yagura`
- other Unix: `$XDG_CONFIG_HOME/yagura`, falling back to `~/.config/yagura`

**`<config root>/profiles` holds only what you changed.** A file there named
after a shipped profile (`codex-cli.toml`) is laid over that profile: it
carries the keys you changed and nothing else, and everything it does not
mention keeps the shipped value -- including whatever a later version of
yagura changes. A file whose id yagura does not ship is a profile of your
own and is read alone. The directory does not have to exist: the GUI's
**Save** in the Models tab and the switch in the Profiles tab create it, and
the file, the first time you change something.

How a file is laid over the shipped profile:

- A table is merged key by key, at every depth: `[model.aliases]` with one
  line changes that one alias and keeps the others, in the shipped order.
- Any other value replaces the shipped one whole: an `args` array you write
  is the whole array, not added to the shipped one.
- `[command]` is a table too, so a key you set does not hide the shipped
  profile's other keys. On Windows a shipped `windows` key still wins over a
  `default` you write (see
  [OS-specific `[command]` selection](#os-specific-command-selection)): set
  the key for your operating system.
- Your file cannot take a table away from the shipped profile. To run
  without one of its tables, disable the shipped profile (see
  [Disabling a profile](#disabling-a-profile)) and write a profile of your
  own under a new id.

The result is checked as one profile. When it does not parse, the profile
is left out of the listing with a warning naming your file; the shipped
profile is not used in its place, since it is not what you asked for.

**A directory given by stage 1 or 2 answers alone**: the shipped profiles
are not laid under it, so it has to hold whole profiles. It is taken as
given -- a directory that does not exist, or a path that turns out to be a
file, leaves the listing empty with a warning rather than falling back to a
different location. This is for running a daemon against a fixed set of
profiles, such as a test fixture.

A shipped profile with nothing to launch on this operating system is never
listed here (see [Availability](#availability)).

A running daemon does not need to be restarted to see a profile change: the
directory is read fresh on every listing request and every launch attempt,
so adding, editing or removing a `.toml` file there takes effect on the next
request.

## Seeing and changing a profile in the GUI

Settings > Profiles opens the details of each profile: every value it uses,
each marked **Shipped** or **Changed**, with the shipped value shown beside
one you changed. Environment values stay hidden until you press **Show**.
What yagura hands the CLI (the MCP server, the status hooks, the model list)
is listed but not edited there.

The command for the operating system you are on is edited one argument per
field, so a space or a quote inside an argument needs no escaping. **Save**
writes it into your file for that profile under this system's own
`[command]` key (`windows`, `macos` or `linux`), so it applies here whatever
`default` the shipped profile has. **Reset to shipped** removes your file's
whole `[command]` table. Model aliases are edited in Plans > Models (see
[Editing aliases in the GUI](#editing-aliases-in-the-gui)).

## Upgrading

A new version's shipped profiles take effect as soon as you run it: new
tables, new aliases and changed defaults all arrive, except the values your
own file sets.

## Schema

```toml
schema = 1                     # required; must be 1
enabled = true                 # optional; defaults to true when omitted. Must
                                # be written before the file's first `[table]`
                                # heading -- inside a table it is either
                                # ignored or excludes the whole profile,
                                # depending on that table's own rule. See
                                # "Disabling a profile" below
display_name = "Codex CLI"     # required; becomes the window's default name,
                                # so it must satisfy the window-label rule
                                # (non-empty, at most 128 UTF-8 bytes, no
                                # control or bidi-override characters)
detection = "codex-cli"        # optional; a detection definition name the
                                # daemon serves (see
                                # crates/yagura-daemon/detection/README.md).
                                # Omitted selects the daemon's own default
                                # definition
# cwd = "C:\\work"              # optional; must be an absolute path. A
                                # relative path or one starting with "~" is
                                # ignored with a warning, not resolved against
                                # anything

[command]                      # optional table; see "[command] omitted" below
default = ["codex"]            # argv used when no operating-system-specific
                                # key below applies
# windows = ["codex"]          # overrides `default` on Windows
# macos = ["codex"]            # overrides `default` on macOS
# linux = ["codex"]            # overrides `default` on any other Unix

[env]                          # optional table; extra environment variables
# API_KEY = ""                 # for the launched process

[model]                        # optional table; a launch option this profile
args = ["-m", "{value}"]       # accepts. `args` is required inside it and must
                                # contain "{value}", which is replaced by the
                                # value a caller asked for. See "Launch
                                # options" below
[model.aliases]                # optional; short name -> the value actually
# fast = "gpt-5.6-luna"        # passed, kept in this file's own order

# [model.choices]              # optional; only under [model]. Where the
# args = ["models"]            # models offered for the aliases come from: a
# format = "lines"             # fixed `values` list, or `args` + `format` to
# separator = "\t"             # ask the CLI itself. See "Model choices" below

[effort]                       # optional; the same shape as [model]
# args = ["-c", "model_reasoning_effort={value}"]

[mcp]                          # optional table; how the launched CLI is
                                # pointed at yagura's MCP bridge. See "MCP
                                # injection" below
args = ["-c", "mcp_servers.yagura.command={bridge_toml}"]
                                # required inside it; appended to the launch
# config_file = '...'          # optional; content of a settings file the
                                # daemon writes for `{mcp_config_file}`

[hooks]                        # optional table; how the launched CLI is made
                                # to report its turns. See "Hooks injection"
                                # below
# args = ["--settings", "{hooks_config_file}"]
# config_file = '...'          # optional; content of a settings file the
                                # daemon writes for `{hooks_config_file}`
```

The identifier is the file name without its `.toml` extension, and must
match `[a-z0-9_-]{1,64}`. A file whose name does not match this pattern, or
whose content fails validation, is left out of the listing entirely, with
one warning line naming it -- it never stops the rest of the directory from
being read.

A profile document fails validation outright (and is excluded) when: `schema`
is not `1`; `display_name` is missing or violates the window-label rule; any
argv element under `[command]` is empty or contains a NUL byte; a
`[model]` / `[effort]` table breaks one of the rules under
[Launch options](#launch-options); or an `[mcp]` or `[hooks]` table breaks
one of the rules under [MCP injection](#mcp-injection) and
[Hooks injection](#hooks-injection). A profile stays valid, with the offending
value dropped and a warning attached, when: `cwd` is relative or starts with
`~`; or an `[env]` key starts with `YAGURA_` (case-insensitively; see
below).

### OS-specific `[command]` selection

For each launch, the argv used is chosen by this operating system's own key
first (`windows`, `macos` or `linux`), falling back to `default`, falling
back to nothing at all -- meaning this profile cannot launch on this
operating system. A profile that only makes sense on one platform, such as a
WSL wrapper, simply omits the others:

```toml
schema = 1
display_name = "WSL: Codex CLI"
detection = "codex-cli"

[command]
windows = ["wsl", "-e", "bash", "-lic", "exec \"$0\" \"$@\"", "codex"]
```

On any other operating system this profile has nothing to launch: it does not
appear in any profile listing there, and naming it directly is refused as an
unknown profile. The `windows` argv itself is
explained under [WSL profiles](#wsl-profiles).

### `[command]` omitted

A profile with no `[command]` table at all launches the daemon's own
resolved default shell (the same one its initial window starts with) --
`shell.toml`, one of the profiles this repository ships, is exactly this:

```toml
schema = 1
display_name = "Shell"
```

If the daemon could not resolve a default shell at start-up, such a profile
is listed as unavailable with a reason saying so, rather than falling back to
anything else.

### `[env]`

Every key is passed to the launched process **verbatim**: values are not
expanded, and an empty string is passed through as an empty value rather than
being treated as absent. Keys beginning with `YAGURA_`, **matched
case-insensitively** (so `yagura_status_token` and `Yagura_Window_Id` are
reserved too), are reserved for the daemon's own injected variables
(`YAGURA_STATUS_TOKEN`, `YAGURA_WINDOW_ID`, `YAGURA_MCP_BRIDGE`); such a key
is dropped, with a warning, rather than being allowed to impersonate the
daemon's own injection.

On a Windows host, a `WSLENV` key is not passed as written. The daemon uses
its value (or, when the profile sets none, the daemon's own `WSLENV`) as the
start of the child's `WSLENV` and adds the names of the variables it
injects into that window -- `YAGURA_WINDOW_ID` and `YAGURA_STATUS_TOKEN`,
`YAGURA_MCP_BRIDGE/p` when a bridge was resolved, and `YAGURA_INSTANCE` when
the daemon itself was started with it -- so they
also reach a process inside a WSL distribution. Items already in the value
are kept in their order and never added twice (compared ignoring ASCII case
and any `/flags` suffix). See [docs/wsl.md](../docs/wsl.md#environment-variables).

A window's terminal is yagura's own, whichever terminal yagura was started
from. So a launched process gets `TERM=xterm-256color` and
`COLORTERM=truecolor`, and none of the variables a terminal sets to name
itself (`TERM_PROGRAM`, `VSCODE_*`, `WT_SESSION`, `KITTY_*`, `TMUX`, and the
like). A profile can still set any of them in `[env]` (for example,
`TERM = "xterm"`).

`[env]` is the last layer of the child's environment, so it wins over every
variable described in [The child's environment](#the-childs-environment).

`[env]` never influences how `argv[0]` is resolved to an executable --
resolution always runs against the environment every child starts from (the
daemon's own environment with the base environment laid over it: its `PATH`
and, on Windows, its extra bin directories), not the profile's.

Do not put secrets in `[env]`: this file is read as plain text from the
config root, and there is no separate secret store a profile can reference
instead.

### `cwd`

Only an absolute path is accepted as a profile's own default working
directory. A relative path, or one starting with `~`, is dropped with a
warning rather than being resolved against the daemon's own working
directory or the user's home -- there is no implicit base to resolve it
against.

## The child's environment

Every process the daemon starts for a profile -- a window's child, and the
command that lists a profile's models -- gets the daemon's own environment
with the following laid over it, a later layer winning for a name it shares
with an earlier one:

1. the **base environment** described below;
2. for a window's child only, `TERM` as the daemon resolved it at startup
   (see [`[env]`](#env));
3. for a window's child only, the `YAGURA_*` variables the daemon injects
   (see [MCP injection](#mcp-injection));
4. for a window's child only, `PATH` with the daemon's report folder in front
   of it (see [Hooks injection](#hooks-injection));
5. the profile's `[env]`.

The layers only add or replace variables: nothing in them removes a
variable the daemon itself inherited.

The base environment is decided once, when the daemon starts, and is the
same for every child until the daemon exits. On Windows it is always empty,
so children see the daemon's own environment as before. On macOS and Linux
it has two parts.

### The login shell's environment

A daemon started outside a terminal (from Finder, the Dock or a desktop
launcher) inherits a minimal environment: typically no locale, and a `PATH`
without anything the user's shell profile adds. So when the daemon's own
`TERM` is missing or empty, it runs the user's login shell once and takes
the environment the shell's startup files leave. A daemon whose `TERM` is
set was started from a terminal, which already handed it that environment,
and skips this step.

- **Which shell.** `$SHELL`, then the login shell recorded for the user in
  the password database, then `/bin/sh`: the first that is an absolute path
  to an executable file. When none is, the step is skipped.
- **How it runs.** As `<shell> -l -i -c` (`csh` and `tcsh` as `<shell> -ic`,
  interactive but not a login shell), with no terminal, in a session of its
  own, in `$HOME` when that is an existing directory. Its environment is the
  daemon's own, without any `YAGURA_*` variable. The command it runs prints
  `env -0` between two markers; whatever the startup files print around
  them is ignored.
- **What is taken.** Every variable it prints, byte for byte, except
  `SHLVL`, `PWD`, `OLDPWD`, `_`, `TERM`, `COLUMNS` and `LINES` (which
  describe the read itself or a terminal) and any name starting with
  `YAGURA_`, matched case-insensitively. A variable the startup files
  `unset` is not removed from children: it keeps the value the daemon
  inherited.
- **Limits.** The read ends as soon as the end marker arrives, so a
  background process a startup file starts cannot hold it up. It has 10
  seconds and 4 MiB of output in total; past either, the shell's whole
  process tree is ended and nothing is taken. A shell that has not exited
  on its own once the environment is read is given the rest of those 10
  seconds to finish, then its process tree is ended. A non-zero exit code
  does not discard an environment that was read.
- **Failure.** The daemon never stops over this step: when the shell cannot
  be started, times out, prints too much, or prints no complete
  environment, children keep the daemon's own environment.

The daemon accepts no connection until this step is over, so its start-up
can be delayed by up to those 10 seconds when a startup file does heavy
work; the GUI waits that long for a daemon it has just started. Each
outcome is logged as one line naming only the reason a step was skipped
(`terminal`, `windows`, `no_shell`), where the shell came from (`env`,
`passwd`, `fallback`), the failure kind (`spawn_failed`, `timed_out`,
`output_too_large`, `no_markers`), the number of variables and the time
taken -- never a shell path, a variable name or a value.

### A locale

When none of `LC_ALL`, `LC_CTYPE` and `LANG` is non-empty as a child would
see it (the login shell's environment, else the daemon's own), the base
environment also sets `LANG` to the first of these that is installed on the
system:

1. on macOS, the language and region chosen in System Settings, as
   `<language>_<REGION>.UTF-8` (for example `ja_JP.UTF-8`);
2. `en_US.UTF-8`;
3. `C.UTF-8`.

This applies whether or not the daemon was started from a terminal. The
chosen value is logged; when none of them is installed, a warning is logged
and children start without a locale. A `LANG` in a profile's `[env]` still
wins. This never happens on Windows.

## Availability

A profile's listed `available` flag, and the reason text beside it when
`available` is `false`, describe one thing only: **whether `argv[0]` can
currently be resolved to something that can be started at all.** It does not
guarantee that launching it will actually succeed -- the child process can
still fail to start or exit immediately for reasons resolution cannot see
(a missing subcommand, a bad working directory, the program itself failing).

An unavailable profile stays in the listing (so it is visible and its reason
is legible) rather than being hidden -- with one exception: a profile with no
argv at all for this operating system (kind 3 below) does not exist here at
all, and is left out of the listing entirely rather than shown as
unavailable (see [OS-specific `[command]` selection](#os-specific-command-selection)).
The reason is always one of four kinds, and never names a path or a user
name:

1. the executable could not be found
2. the command line contains a character the command interpreter would
   reinterpret (Windows `.cmd` / `.bat` shims only; see below)
3. no argv is defined for this operating system
4. no default shell was resolved when the daemon started (`[command]`
   omitted profiles only)

## Disabling a profile

A profile's top-level `enabled` key (see [Schema](#schema)) switches it off
without deleting it. Turn it off from the GUI's Settings panel -- the
Profiles tab has a switch for every profile -- or by hand, by writing
`enabled = false` into your file for that profile before its first
`[table]` heading. Switching it back on from the GUI removes the key again.
Omitting the key, or writing `enabled = true`, means enabled; a profile
written before this key existed is unaffected.

A disabled profile stays exactly where it is: no file is moved or
deleted, and the profile keeps its place in the GUI's Settings panel, in
`roles.toml` and in a plan that already names it. What it loses is being
offered: it drops out of every other profile list, including the MCP tool
[`list_profiles`](../docs/mcp-setup.md#list_profiles), and naming it to open
a window -- `open_window` with `profile`, `send_handoff`, `open_task_window`
-- is refused as `profile_disabled` (see
[Error Handling](../docs/mcp-setup.md#error-handling)). A role row or a plan
task that already names a disabled profile still saves and still loads; only
opening a window through it is refused, and only at that point.

Two paths sit outside this switch entirely, because neither goes through a
profile id: `open_window`'s `command` form launches whatever argv it is
given directly, with no profile in the request to check (see
[`open_window`](../docs/mcp-setup.md#open_window)); and a window already
open when its profile was switched off keeps running regardless. Restoring a
saved session does not reopen a window whose profile is now disabled from
its saved argv either -- see
[Restoring a window opened from a profile](#restoring-a-window-opened-from-a-profile).

**Downgrading**: an older build's profile parser does not know about
`enabled` and reads past it without complaint, so a profile you disabled
comes back as a usable profile -- in the GUI and for agents -- once you run
an older daemon against the same profiles directory. To keep such a profile
from launching under an older build, do not use that CLI, or return to a
newer build.

## Windows resolution

When `argv[0]` contains no path separator, resolution searches, in order,
the daemon's own `PATH` directories and then its extra bin directory
(`%APPDATA%\npm` -- where npm installs its global package shims, which its
installer does not always add to `PATH`). Within each directory, the rule is
asymmetric by whether `argv[0]` already carries an extension: a name that
does (`codex.exe`) tries only that literal name, while an extensionless name
(`codex`) tries `.exe`, then `.cmd`, then `.bat` and **never** matches a
same-name extensionless file (npm installs a POSIX shell shim under exactly
that name next to the `.cmd` shim it can start; probing it would resolve to
something `CreateProcess` cannot launch). A weaker match found in an earlier
directory wins over a stronger one found in a later directory, matching the
order the operating system itself would search.

Package managers other than npm that also install shims outside `PATH` --
pnpm, Volta -- are not searched separately; add their shim directory to the
daemon's `PATH` instead, and it is found the same way.

An `argv[0]` that already contains a path separator (an absolute path, or a
relative one) is never searched for this way: it is taken exactly as
written, and whether it can launch is judged directly against that literal
path. If that path ends in `.cmd` or `.bat` (in any letter case), it is
still started through the command interpreter with the same wrapping and
the same rejection rules as a shim found by name (see below).

### `.cmd` / `.bat` shims

An npm-installed CLI's shim is a `.cmd` or `.bat` script, which the
operating system cannot start directly -- only the command interpreter can.
When resolution finds one, or `argv[0]` is a path to one, it is started through
`%SystemRoot%\System32\cmd.exe /d /c <shim path> <args...>`
(`/d` suppresses the interpreter's AutoRun registry hook; the interpreter's
location always comes from `%SystemRoot%`, never from the user-writable
`COMSPEC` variable). This wrapping is transparent: a profile's argv is
written the same way whether it ultimately resolves to a plain executable
or a shim.

When both the shim path and at least one trailing argument contain a space
(or otherwise need their own quoting), a bare `call` is inserted right
before the shim path. This works around a quirk of `cmd.exe`'s own command
line preprocessing that would otherwise mangle the shim path's quoting; it
changes nothing about what a profile writes.

**The resolved shim path, every trailing argument, and the command interpreter's
own resolved path are rejected if any of them contains a character the command
interpreter would reinterpret** (`%` `&` `|` `<` `>` `^` `"`, or a newline).
Whitespace by itself is fine and does not trigger this: a shim path or argument
containing a space is passed through whole and reaches the child exactly as
written. When a rejection does happen, launching that profile fails as reason
kind 2 above, and there is no way to escape the disallowed character through
this wrapping -- **write the resolved executable's own absolute path instead of
the shim's bare name** (for example
`C:\Program Files\nodejs\node_modules\...\codex.js` run through `node`, or
whatever the shim itself wraps) if an argument genuinely needs one of those
characters.

## Unix

`argv[0]` is looked up in the `PATH` directories of the environment every
child starts from -- the base environment's `PATH` when it has one (see
[The child's environment](#the-childs-environment)), the daemon's own
otherwise -- for an exact, executable match; there is no extension probing
and no shim wrapping. The argv a profile carries is **never rewritten** on
Unix -- what a profile names is exactly what is spawned.

## Launch options

A profile can declare how the two launch options a caller may ask for --
`model` and `effort` -- reach the CLI it starts. Each is an optional table
named after the option, and a profile that omits one does not accept it:
asking for it is refused as `invalid_argument` before anything is started.

```toml
[model]
args = ["-m", "{value}"]

[model.aliases]
fast = "gpt-5.6-luna"
default = "gpt-5.6-terra"
strong = "gpt-6-astra"

[effort]
args = ["-c", "model_reasoning_effort={value}"]
```

`args` is the argv this option appends, and the array as a whole must
contain `{value}` at least once. Every occurrence of `{value}`, in every
element, is replaced by the resolved value -- always *inside* an element, so
a value can never add, split or reorder argv elements however it is written.
`{value}` is the only placeholder there is, and there is no escaping syntax.

`aliases` is optional and maps a short name to the value actually passed.
Alias keys use the same character set as a profile id (`[a-z0-9_-]{1,64}`);
alias values follow the launch option value rule (ASCII letters, digits and
`. _ - : / + @`, not starting with `-`, at most 128 bytes) because they
become argv elements; the same rule applies to a caller's value. A value that
matches no alias key is passed through unchanged, so a CLI's own full model
name always works. Resolution is one step deep: an alias's value is never
looked up as an alias again. The table is reported -- by
[`list_profiles`](../docs/mcp-setup.md#list_profiles) and by the GUI -- in
this file's own order, not sorted.

A malformed `[model]` / `[effort]` table excludes the whole profile from the
listing, with one warning line naming the file: when `args` is missing, when
the `args` array contains no `{value}`, when an alias key or value breaks the
rules above, when a `[model.choices]` table breaks one of the rules under
[Model choices](#model-choices), or when the table carries any key other
than `args`, `aliases` and -- under `[model]` only -- `choices` (so a
misspelt `aliases` cannot silently mean "no aliases"). An
empty argv element, or one containing a NUL byte, is excluded exactly as it
is under `[command]`.

What a launch appends comes after the profile's own argv -- `model` first,
then `effort`, each in its own `args` order, and then any
[MCP injection](#mcp-injection). An option the caller left out appends
nothing at all, so the CLI's own default applies. The WSL profiles this
repository ships hand every appended element to the CLI as its own argv
element (see [WSL profiles](#wsl-profiles)). On top of that, the launch
option value rule admits none of the characters `cmd.exe` would reinterpret
when it starts a `.cmd` / `.bat` shim, so a value reaches the child exactly
as written on that path too.

On Windows, a value outside the launch option value rule is refused as
`invalid_argument` before any process starts, so it never reaches the
`.cmd` / `.bat` shim check described in
[`.cmd` / `.bat` shims](#cmd--bat-shims). A profile's listed `available`
flag says nothing about a launch option's value: it is judged against the
argv with **no** launch option applied, so "available" and "this particular
value cannot be launched" are both true at once.

### Recommended aliases

Every profile this repository ships declares `fast`, `default` and `strong`.
Putting the same three names in a profile you write yourself lets whoever
asks for a window pick a tier without knowing which CLI, or which model
names, are behind it:

| profile | `model` aliases | `effort` |
|---|---|---|
| `claude-code`, `wsl-claude-code` | `fast` = `haiku`, `default` = `sonnet`, `strong` = `opus`, `fable` = `fable` | accepted, no aliases (levels pass through) |
| `codex-cli`, `wsl-codex-cli` | `fast` = `gpt-5.6-luna`, `default` = `gpt-5.6-terra`, `strong` = `gpt-6-astra` | accepted, no aliases (levels pass through) |
| `antigravity-cli`, `wsl-antigravity-cli` | `fast` = `gemini-3.8-flash-low`, `default` = `gemini-3.8-flash-medium`, `strong` = `gemini-3.1-pro-high` | not accepted (see below) |
| `shell` | accepts neither option | -- |

Those mappings come from each CLI's own `--help` (Claude Code 2.1.263, Codex
CLI 0.153.4, Antigravity CLI 1.2.11) and, for the reasoning levels and the
model names, the Codex configuration and models reference. **Model names
change over time**: read the table as the state when these profiles were
written, and edit your own copies to whatever your CLI and your account
actually accept -- yagura never validates a model name, it only passes it
on. The `antigravity-cli` aliases were confirmed only against Antigravity's
free-tier "Starter Quota"; a model a paid tier can select was not checked.

Antigravity CLI (`agy`) has an `--effort` flag, but it cannot be combined
with the *effort-qualified* model ids `agy models` lists: `--model
gemini-3.8-flash-low --effort high` is refused as a conflict, and an
unqualified id such as `gemini-3.8-flash` is accepted only together with
`--effort`. A `[model]` alias and an `[effort]` alias are chosen
independently -- either may be absent -- so an `[effort]` table would leave
some combinations refused. `antigravity-cli` and `wsl-antigravity-cli`
therefore select the reasoning level through effort-qualified model ids and
ship no `[effort]` table at all -- asking either for `effort` is refused the
same way `shell` refuses `model`. An interactive `agy` does not fail on a
refused selection: it warns and falls back to its own default model, which
is the outcome this profile's `[model]` aliases exist to avoid.

### Model choices

A `[model]` table may carry a `[model.choices]` table saying where the
models offered for its aliases come from. It only shapes what the GUI's
Models tab offers (see [Editing aliases in the GUI](#editing-aliases-in-the-gui));
a launch never reads it, so a value that is not in the list is still passed
on exactly as before. It takes one of two forms:

```toml
# A fixed list, offered in this order
[model.choices]
values = ["haiku", "sonnet", "opus", "fable"]

# Ask the CLI itself: one model per line
[model.choices]
args = ["models"]
format = "lines"
separator = "\t"

# Ask the CLI itself: one JSON document
[model.choices]
args = ["debug", "models"]
format = "json"
items = "/models"
id = "/slug"
label = "/display_name"
filter = { pointer = "/visibility", equals = "list" }
```

The rules:

- Exactly one of `values` and `args` is present.
- `values` is a non-empty array. Every element follows the launch option
  value rule and appears only once, because a picked model is saved as an
  alias's value. No other key may sit beside it.
- `args` is a non-empty array of argv elements, none of them empty or
  containing a NUL byte. `format` is then required, and is `"lines"` or
  `"json"`:
  - `"lines"` requires `separator`, a non-empty string, and takes none of
    the JSON keys below.
  - `"json"` requires `items` and `id`, and may add `label` and `filter`;
    it takes no `separator`. `items`, `id`, `label` and `filter.pointer` are
    JSON Pointers (RFC 6901): either empty or starting with `/`. `filter`
    is an inline table with both `pointer` and a non-empty `equals`.
- Any other key is refused.

A table breaking any of these excludes the whole profile from the listing,
with one warning line naming the file, the same as the other launch option
rules. `[effort]` has no choices: a `choices` key under it is an unknown key
and excludes the profile the same way.

Of the profiles this repository ships, `claude-code` and `wsl-claude-code`
carry a fixed `values` list, `codex-cli` and `wsl-codex-cli` ask Codex CLI
(`codex debug models`, JSON), and `antigravity-cli` and
`wsl-antigravity-cli` ask Antigravity CLI (`agy models`, one model per line,
id and display name separated by a tab). Codex CLI's `debug` command may
change its output; if it does, edit the pointers to follow it.

**How the list is taken.** The daemon runs the listing command only when
the GUI asks for it -- when a profile is shown in the Models tab, or its
refresh button is pressed; no MCP tool lists models or edits aliases. The
command is the profile's own launch argv for this operating system, found
exactly as a window's is (including `.cmd` / `.bat` shims and the
characters they refuse), followed by `args`. No launch option and no MCP
injection is added. It runs once, outside any window: with no terminal and
no console window, its standard input closed, its standard output read, and
its standard error not read at all. It gets the daemon's own environment
with the base environment and then the profile's `[env]` (reserved keys
dropped) laid over it (see
[The child's environment](#the-childs-environment)), but not the `TERM` or
the `YAGURA_*` variables a window's child is given; it starts in the
profile's `cwd`, or else in the daemon's own working directory.

The command gets 20 seconds and 4 MiB of standard output; past either, its
whole process tree is ended and the list is not taken. Its process tree is
ended when it finishes, too. At most two listing commands run at once across
the daemon, and a request beyond that is refused rather than queued.

**How the output is read.** For `"lines"`, every non-empty line (a trailing
`\r` dropped) is one model: the text before the first `separator` is its
id, the text after it its display name, and a line without the separator is
an id alone. For `"json"`, the whole standard output is one JSON document;
`items` points at the array holding one element per model, and `id` and
`label` point, inside one element, at strings. `filter` keeps only the
elements whose value at `pointer` is exactly the string `equals`.

An id that breaks the launch option value rule is skipped, and so is a JSON
element with no string at `id`. A display name that would not be accepted
as a window name is dropped and the id kept. A repeated id keeps its first
occurrence. More than 256 models refuses the whole list rather than showing
part of it.

**When no list can be taken**, the Models tab shows one line, `Could not
read the model list: <reason>. You can still type model names.`, and every
alias gets a text field instead. The reason is a fixed phrase that never
quotes the command's output:

- `<program> was not found` -- the command could not be started, or it
  exited with status 127, a shell's "command not found". In the second case
  the name is the last element of the profile's launch argv, so behind a
  launcher such as `wsl` it is the CLI the shell could not find.
- `exited with status <n>`, or `terminated without reporting an exit code`
- `did not finish within 20 seconds`
- `output too large`
- `output did not match the profile's format` -- the output is not UTF-8,
  not JSON for `"json"`, `items` does not point at an array, or no model is
  left once it is read
- `lists more models than accepted`
- one of the [availability](#availability) reasons, when the profile cannot
  be launched on this host at all

The GUI stops waiting after 25 seconds and shows `the request timed out` as
the reason.

### Editing aliases in the GUI

The Plans panel's **Models** tab, beside Roles, shows every listed profile
whose `[model]` table declares at least one alias. For the chosen profile it
shows one row per alias, in the file's order: the alias, its current value,
and the value the same-id profile shipped with yagura gives it (blank when
yagura ships no such profile). A value that differs from the shipped one is
marked `edited`, and its reset button puts the shipped value back. A value
is picked from the model list, or typed after choosing `Other…`; with no
list, it is typed.

Only the values change. The tab never adds, removes or renames an alias,
and the daemon refuses a save whose aliases are not exactly the profile's.
**Save** writes to your file for that profile in the daemon's profiles
directory (see [Where profiles live](#where-profiles-live)), creating it when
there is none, never to this repository's copy. Only the aliases that
changed touch the file: a value put back to the shipped one is removed from
your file, so a later version's default reaches it again, and any other
value is written into your file's `[model.aliases]`. Comments, spacing, key
order, other tables and the file's line endings stay as they were. Every
value is checked against the launch option value rule, and the result is
read back, laid over the shipped profile, before it replaces the old file;
nothing is written when no value changed. **Discard** drops the unsaved
edits.

A saved value applies from the next window opened from that profile; a
window already open keeps what it was started with. Against a daemon whose
wire revision is older than `10`, the tab is read-only.

## MCP injection

A profile's optional `[mcp]` table tells the daemon how to point the CLI it
starts at yagura's own MCP bridge, `yagura-mcp`, so a window opened from the
profile can call yagura's MCP tools without anything being registered in
that CLI (see [docs/mcp-setup.md](../docs/mcp-setup.md#windows-yagura-opens)).
A profile without the table gets no injection: that is how a profile says
its CLI has no way to be handed an MCP server at launch (`shell` is one).
`antigravity-cli` and `wsl-antigravity-cli` are another: Antigravity CLI
(`agy`) reads MCP servers only from its own global registration, not from
anything passed at launch, so there is no per-launch injection point to fill
in. Reaching yagura's MCP bridge from an `agy` window means registering it
once -- from Settings > Integrations in the GUI (see
[MCP registration](#mcp-registration) below), or by hand with `agy mcp add`,
see [docs/mcp-setup.md's "Configuring Antigravity CLI"](../docs/mcp-setup.md#configuring-antigravity-cli).

```toml
# Claude Code on Windows: the server definition goes into a settings file
[mcp]
config_file = '{"mcpServers":{"yagura":{"command":"${YAGURA_MCP_BRIDGE}","args":[]}}}'
args = ["--mcp-config", "{mcp_config_file}"]

# Codex CLI on Windows: -c overrides written as TOML literal strings
[mcp]
args = [
  "-c", "mcp_servers.yagura.command={bridge_toml}",
  "-c", "mcp_servers.yagura.args=[]",
  "-c", "mcp_servers.yagura.env_vars={env_names_toml}",
  "-c", "mcp_servers.yagura.tool_timeout_sec=86400",
]
```

The table takes two keys, and any other key excludes the profile:

- `args` (required): argv elements appended to the launch, after the
  profile's own argv and its launch options.
- `config_file` (optional): the content of a settings file for the CLI to
  read, written by the daemon exactly as given -- no placeholder is
  substituted inside it.

Three placeholders are substituted inside `args` elements, always within an
element, so they can never add, split or reorder argv elements:

| placeholder | replaced by |
|---|---|
| `{bridge_toml}` | the bridge's absolute path as a TOML literal string, for example `'C:\path\to\yagura-mcp.exe'` |
| `{env_names_toml}` | the TOML literal-string array `['YAGURA_WINDOW_ID', 'YAGURA_STATUS_TOKEN', 'YAGURA_INSTANCE', 'YAGURA_MCP_BRIDGE']` |
| `{mcp_config_file}` | the path of the file the daemon wrote from `config_file`; usable only when the table has `config_file` |

An element with none of them passes through unchanged. `${NAME}` is not a
placeholder: it is left for the CLI to expand from its own environment,
which is how the bundled Claude Code tables name the bridge
(`${YAGURA_MCP_BRIDGE}`). The whole profile is excluded, with one warning
line naming the file, when `args` is missing, when an element carries any
other `{name}`-shaped token (ASCII letters, digits and `_` between braces),
or when `{mcp_config_file}` is used without `config_file`.

What the daemon provides around the table:

- **The bridge.** At start-up the daemon looks for `yagura-mcp` next to its
  own executable (the installer ships both side by side). When there is none
  usable, it logs a warning and no window gets an injection.
- **The environment.** Every window's child gets `YAGURA_MCP_BRIDGE`, the
  bridge's absolute path, next to `YAGURA_WINDOW_ID` and
  `YAGURA_STATUS_TOKEN`. The token only ever travels in the environment:
  neither the injected argv nor a settings file carries it.
- **The settings file.** `config_file` is written under the instance's
  state directory, in `mcp/`, as `mcp-<SHA-256 of the content>.json`. A file
  already there under that name is not written again.

The injection applies to every launch from a profile -- `open_window` with
`profile`, a `send_handoff` that opens a new window from a `profile`,
`open_task_window`, and the GUI's add-window form with a profile selected --
and to none from a raw argv. It is built again for each launch and is not
recorded with the window, so a saved session never carries it.

The window still opens, without the injection and with a warning in the
daemon log, when the injection cannot be carried out:

- the bridge's path contains a `'` or a newline, which a TOML literal string
  cannot hold;
- the table has `config_file` but no settings directory was resolved, or
  the file cannot be written;
- `argv[0]` resolves to a `.cmd` / `.bat` shim (see
  [`.cmd` / `.bat` shims](#cmd--bat-shims)) and a rendered element contains
  one of the characters the command interpreter would act on (`%` `&` `|`
  `<` `>` `^` `"`, or a newline). Unlike the profile's own argv, which is
  refused outright, the injection is dropped and the window opens without
  it.

The last rule is why the bundled tables look the way they do. npm installs
Claude Code and Codex CLI as shims, so the Windows `claude-code` profile
puts its JSON, which needs `"`, in a settings file, and `codex-cli` spells
its values as TOML literal strings, which need none. The WSL profiles start
`wsl.exe`, which is not a shim, so `wsl-claude-code` passes the same JSON
straight on the command line.

Where a CLI already has a server named `yagura` configured, the bundled
tables take over for a window yagura opens. Claude Code starts the
`--mcp-config` definition instead of a same-named one from a project's
`.mcp.json`. Codex CLI merges `-c` overrides key by key, so keys of an
existing `[mcp_servers.yagura]` table that the injection does not set stay
in effect. The bundled Codex tables set `tool_timeout_sec` to a day: Codex's
own 60-second default would otherwise end a `wait_for_status`,
`wait_for_report` or `send_handoff` call that is still waiting on the daemon.

## Hooks injection

A profile's optional `[hooks]` table hands the launched CLI the hooks that
make it report its turn boundaries to the daemon with
`yagura-daemon --emit-status` (what the reports do is in
[Structured sources](../crates/yagura-daemon/detection/README.md#structured-sources---emit-status)).
It follows the `[mcp]` rules above -- the same `args` and `config_file` keys,
the same settings directory, argv order and drop rules -- with three
differences:

- The one placeholder is `{hooks_config_file}`, the path of the file written
  from `config_file` (usable only when the table has `config_file`). The
  file is named `hooks-<SHA-256 of the content>.json`.
- It needs no bridge: a profile's hooks are injected whether or not
  `yagura-mcp` was found.
- Its elements are appended after the `[mcp]` ones. When `argv[0]` resolves
  to a `.cmd` / `.bat` shim, each table is judged on its own: a `[hooks]`
  element with a character the command interpreter would act on drops only
  the hooks.

```toml
# Claude Code on Windows: hooks go into a settings file
[hooks]
config_file = '{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"yagura-daemon --emit-status waiting_input --source claude-code"}]}]}}'
args = ["--settings", "{hooks_config_file}"]

# Codex CLI: -c overrides whose values are TOML literal strings, free of `"`
[hooks]
args = [
  "-c", "hooks.Stop=[{hooks=[{type='command',command='yagura-daemon --emit-status waiting_input --source codex-cli'}]}]",
]
```

**The daemon's report folder.** A hook names the daemon as `yagura-daemon`
with no path. At start-up the daemon places its own executable (a hard link
where the file system allows one, a copy otherwise) as `yagura-daemon` --
`yagura-daemon.exe` on Windows -- in `bin` under the instance's state
directory, replacing what an earlier start left there, and puts that folder in
front of every window's `PATH`. The folder holds that one file and nothing
else; the installation folder is not used, because it also holds files (an
uninstaller) a shell in a window must not find by name. When the folder cannot
be prepared, the daemon logs one warning and windows open with `PATH`
unchanged. A profile's own `[env]` `PATH` replaces the one the daemon builds.
A WSL profile's hooks run inside the distribution, where the bare name is not
resolved, so they call `yagura-daemon.exe`.

**A hook that depends on registration.** When the profile's
`[mcp_registration]` owns settings-file entries (a `toml_set.entries`, or a
`json_entry`; see [MCP registration](#mcp-registration)), the daemon reads them
back each time a window opens and injects the `[hooks]` table only while every
one is in place. This is how the bundled Codex profiles work: Codex runs a
hook given with `-c` only once its trust is recorded in `~/.codex/config.toml`,
and the key that records it carries characters a shim refuses, so
registration writes the trust and the launch injects the hooks. A profile with
no `[hooks]` table but a `json_entry` -- Antigravity CLI, which reads hooks
only from its own file -- injects nothing; the entry is the hook.

**Which windows expect reports.** A window whose `[hooks]` elements were
actually appended to its launch, and a window whose profile's `json_entry` was
in place when it opened, expect their CLI to report. Such a window is marked
in the GUI when no report arrives (see
[When reports are missing](../crates/yagura-daemon/detection/README.md#when-reports-are-missing)).
A table that was dropped on the way -- no settings directory, a file that
could not be written, a shim, entries not in place -- expects nothing.

The bundled profiles map these events (the versions are those the profiles
were checked with):

| Profiles | `running` | `waiting_approval` | `waiting_input` |
| --- | --- | --- | --- |
| `claude-code`, `wsl-claude-code` (Claude Code 2.1.285) | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `PermissionRequest` | `Stop`, `StopFailure` |
| `codex-cli` (Codex CLI 0.159.2), `wsl-codex-cli` (0.158.0) | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `PermissionRequest` | `Stop`, `Interrupt` |
| `antigravity-cli`, `wsl-antigravity-cli` (Antigravity CLI 1.2.14, through `json_entry`) | `PreInvocation`, `PostToolUse` | -- | -- |

`wsl-claude-code` passes the JSON on the command line, since the Windows path
of a settings file is unreadable from inside WSL. Changing a Codex hook's
command changes the hash Codex trusts it by, so the matching
`toml_set.entries` value has to be taken again (see
[MCP registration](#mcp-registration)).

## MCP registration

A profile's optional `[mcp_registration]` table tells the daemon how to
register its own MCP bridge with the CLI's own registry, the way `<cli> mcp
add` (or the CLI's own equivalent) would -- from Settings > Integrations in
the GUI, rather than by hand every time. A profile without the table offers
no row there: that is how a profile says its CLI has no registry of its own
to add to (`shell` is one).

```toml
[mcp_registration]
group = "WSL"                  # optional; the GUI's heading for this row.
                                # Omitted, a row is listed under the host
                                # environment's own group
add = ["mcp", "add", "{env_args}", "{server}", "--", "{bridge}"]
remove = ["mcp", "remove", "{server}"]
env_arg = ["--env={env}"]      # optional; required whenever a named
                                # instance's environment has to reach `add`
bridge_probe = ["wsl", "-e", "wslpath", "-u", "{bridge}"]
                                # optional; converts the bridge's path into
                                # this environment's own before `add` uses it
env_forward = { name = "WSLENV", separator = ":" }
                                # optional; names the variable a named
                                # instance's environment must also be listed
                                # in for `add`'s pairs to reach the process

[mcp_registration.status]      # required; how a row's current state is read
args = ["mcp", "get", "{server}", "--json"]  # exactly one of args / file
format = "json"                 # "json" or "lines"
command = "/transport/command"  # JSON Pointer ("json") or line prefix ("lines")
args_pointer = "/transport/args"  # optional; "json" only
require = { "Scope:" = "User" }   # optional; "lines" only
absent = "No MCP server named"    # optional; a substring meaning "not registered"

[mcp_registration.toml_set]    # optional; an edit made once, right after add
file = ".codex/config.toml"    # home-relative
table = ["mcp_servers", "{server}"]
values = { tool_timeout_sec = 86400 }
confirm = { tool_timeout_sec = "/tool_timeout_sec" }

[mcp_registration.toml_set.entries]
                                # optional; tables of the same file the
                                # registration owns outright: created by
                                # Register, deleted by Remove
under = ["hooks", "state"]     # where they live; no placeholders
[mcp_registration.toml_set.entries.windows]
                                # one set per OS: windows / macos / linux
                                # override default, chosen like [command]
'C:\<session-flags>\config.toml:stop:0:0' = { trusted_hash = "sha256:..." }
[mcp_registration.toml_set.entries.default]
'/<session-flags>/config.toml:stop:0:0' = { trusted_hash = "sha256:..." }

[mcp_registration.json_entry]  # optional; top-level entries of a JSON
                                # settings file the registration owns outright
file = ".gemini/config/hooks.json"  # home-relative
[mcp_registration.json_entry.entries."{server}"]
PreInvocation = [{ type = "command", command = "{daemon} --emit-status running --source antigravity-cli", timeout = 10 }]

[mcp_registration.files]       # optional; how a home-relative status.file,
                                # toml_set.file or json_entry.file is read and
                                # written inside another environment (WSL)
                                # rather than directly
read = ["wsl", "-e", "sh", "-c", "...", "sh", "{file}"]
replace = ["wsl", "-e", "sh", "-c", "...", "sh", "{file}"]  # optional
absent_exit = 3                 # optional; the read tool's exit code for a
                                 # missing file, told apart from every other
                                 # failure
```

Five placeholders are substituted inside `[mcp_registration]`'s argv, always
*within* an element so none of them can add, split or reorder one:
`{server}` (the registration name -- `yagura`, or `yagura-<instance>` for a
named instance), `{bridge}` (the bridge's path, or this environment's own
path to it once `bridge_probe` converts it), `{env_args}` (only as a whole
`add` element; expands to zero or more environment pairs, `env_arg` applied
to each), `{env}` (only inside `env_arg`; one `NAME=value` pair), and
`{file}` (only inside `files.read` / `files.replace`; a `status.file`,
`toml_set.file` or `json_entry.file` path, resolved against `$HOME` by the
tool itself, never spliced into a script string).

**Owned entries.** `toml_set.entries` and `json_entry` describe settings a
registration adds and a removal takes out again, for a CLI that runs yagura's
hooks only once something is written into its own files (see
[Hooks injection](#hooks-injection)).

- `toml_set.entries` writes one table per key of the set chosen for the
  host operating system, under `under` -- the bundled Codex profiles write
  `[hooks.state."<key>"]` with its `trusted_hash`, the hash Codex itself
  reports for that hook (`codex app-server`, request `hooks/list`); a hook
  whose command changes needs its hash taken again. A set is picked the way
  [`[command]`](#os-specific-command-selection) picks an argv: `windows`,
  `macos` or `linux` when present, `default` otherwise. A WSL profile, whose
  CLI runs on Linux, carries `default` alone. No placeholder is substituted in
  it. Each entry must be a non-empty table, and at least one set must be
  present. Missing tables on the path are created; a key on the path that
  holds something other than a table fails the step and nothing is written.
- `json_entry` writes top-level entries of a JSON object: `file` is
  home-relative and `entries` names each one. An entry's name may carry
  `{server}`; its value is any TOML value, written as JSON, whose strings may
  carry `{daemon}`. `{daemon}` is replaced by the absolute path of the
  daemon's copy in its report folder -- converted by `bridge_probe` when the
  row has one -- and a path containing whitespace, `'`, `` ` ``, `$`, or a
  character the command interpreter would act on is not written at all, since
  a hook command carries it unquoted. The rest of the file is kept byte for
  byte, a missing file is created holding only these entries, and a file that
  is not a JSON object is left untouched with the step failed.

Both edits read and write through `files.read` / `files.replace` when the row
has them, and are written atomically otherwise, the same way `toml_set` is.

Put `{env_args}` before `{server}` and keep each flag and its value in one
element (`--env={env}`): some CLIs reject a flag after the server name, and
a flag that takes several values would otherwise take the name as one.

`add`, `remove` and `status.args` run the same way any other window from
this profile does: through the profile's own launch argv for this operating
system, the way [Model choices](#model-choices) already runs a listing
command -- so a WSL profile's registration reaches the same CLI its own
`~/.bashrc` would put on `PATH`. **The last element of that launch argv is
always the executable itself**, the same assumption
[Launch options](#launch-options) already makes to append `model` /
`effort`; a `[mcp_registration]` table adds nothing that would move it.
`bridge_probe`, `files.read` and `files.replace` are different: they run as
their own standalone commands, because they have nothing to do with
launching the CLI.

**Reading a row's state.** `status` names either a command to run (`args`,
read the same way a window's launch is) or a settings file to read (`file`,
home-relative; inside another environment, through `files.read` instead of
directly). The result is one of five states: **registered** (the CLI's own
record names exactly this bridge, with every `require` / `toml_set.confirm`
value and every owned entry in place), **differs** (the record exists but
names another bridge, is missing a required value, or lacks an owned entry or
holds another value in it -- `differs_in` names which, giving owned entries
as `under` joined by `.` or as the `json_entry` names), **not registered**
(`absent` matched the command's output, the settings file does not exist, or
a `files.read` tool exited with `absent_exit`), **not installed** (the CLI
itself, or its `bridge_probe`, could not be found or run), or **unknown**
(the output could not be read as the declared format -- never guessed as
registered or not).

**Update and Remove can drop a hand-written value.** A CLI's own `add`
command can rewrite its whole record rather than editing it in place: the
bundled `codex-cli` and `wsl-codex-cli` tables' `add` drops any key its own
`-c mcp_servers.yagura.*` overrides did not set, including a
`tool_timeout_sec` an earlier `toml_set` edit wrote -- which is why their
`[mcp_registration.toml_set]` writes it back once, right after `add`. Update
runs `remove` then the same steps as Register. Remove runs `remove`, then
takes out the owned entries (`toml_set.entries`, `json_entry`); it leaves
`toml_set.values` alone.

## WSL profiles

The three WSL profiles this repository ships (`wsl-claude-code`,
`wsl-codex-cli`, `wsl-antigravity-cli`) start the CLI with

```toml
[command]
windows = ["wsl", "-e", "bash", "-lic", "exec \"$0\" \"$@\"", "claude"]
```

`wsl -e` runs `bash` directly rather than through the distribution's
default shell. `bash -lic` is both a login and an interactive shell, so
both `~/.profile` and `~/.bashrc` run, and the `PATH` they set together --
including an addition a tool such as nvm makes only in `~/.bashrc`, which a
plain login shell does not read -- is used to find the CLI, and its fixed
script `exec "$0" "$@"` replaces bash with the CLI: the element after the script
becomes `$0` (the CLI's name) and every later element -- launch options and
the MCP injection -- reaches the CLI as one argv element each, never split
or expanded. The distribution needs `bash`, and the Linux build of the CLI
installed inside it; see [docs/wsl.md](../docs/wsl.md).

## Approval-skip flags

Some CLIs accept a flag that skips their own interactive approval prompts
(for example Claude Code's `--dangerously-skip-permissions`, or Codex CLI's
`-a never`). **None of the profiles this repository ships enable one -- that
is a deliberate choice, since shipping a default that bypasses a CLI's own
safety prompts is not something to do silently.** If you want a profile that
does, write your own -- but read that CLI's own documentation for what the
flag does first:

```toml
schema = 1
display_name = "Claude Code (skip permissions)"
detection = "claude-code"

[command]
default = ["claude", "--dangerously-skip-permissions"]
```

## Restoring a window opened from a profile

The session snapshot records, for a window opened from a profile, the
profile's id and the `model` / `effort` values it was opened with, next to
the argv, detection definition and `vendor` it was spawned with. When the
GUI restores such a window, it opens it **from the profile again**, with the
same working directory, name and launch option values, so edits to the
profile since -- and its MCP injection -- apply to the restored window.

Only when the daemon refuses that launch (for example, the profile no longer
exists, or no longer runs on this operating system) does the GUI fall back
to re-spawning the saved argv, detection definition and `vendor` as they
were recorded; a restored window opened that way gets no MCP injection --
except when the refusal is `profile_disabled` (see
[Disabling a profile](#disabling-a-profile)): retrying the saved argv would
launch the very CLI the user switched off, so that window is left unopened
instead, and its name is collected into a single notice covering every
window the current restore held back this way. Any other failure is not
retried. A window opened from a raw argv, or recorded by a daemon from
before the snapshot kept the profile, is always restored from its saved
argv.
