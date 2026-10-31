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

An older daemon does not know the keys a later release adds. A profile of
your own that uses one -- for example `env` under `[model]` or `[effort]`,
`launch_context` under `[delivery]`, `notice` under `[mcp_registration]`,
`items` and `name` under `[mcp_registration.status]`, the `[credentials]`
table, the `[hook_events]` table, or `retired` under
`[mcp_registration.toml_set.entries]` -- is left out of its
listing with a
warning. A `{server}` in a `json_entry` `file` is no placeholder to such a
daemon: it is taken as part of the file's name.

A profile's id -- its file name without `.toml` -- is part of every hook
that calls `--hook` for it (see [Hook events](#hook-events)), and those
hooks can outlive a launch in the CLI's own settings. Renaming the file
leaves them naming an id the daemon no longer finds, so they do nothing
until the CLI is registered again.

## Schema

```toml
schema = 1                     # required; must be 1
enabled = true                 # optional; defaults to true when omitted. Must
                                # be written before the file's first `[table]`
                                # heading -- inside a table it is a key that
                                # table does not take, and excludes the whole
                                # profile. See "Disabling a profile" below
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
args = ["-m", "{value}"]       # accepts. `args`, `env` or both; "{value}"
                                # must appear in one of them, and is replaced
                                # by the value a caller asked for. See "Launch
                                # options" below
# env = { CLI_MODEL = "{value}" }
                                # optional; variables set for the launch
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
args = ["-c", "mcp_servers.{server}.command={bridge_toml}"]
                                # required inside it; appended to the launch
# config_file = '...'          # optional; content of a settings file the
                                # daemon writes for `{mcp_config_file}`
# config_dir = { "a.json" = '...' }
                                # optional; files of a settings directory the
                                # daemon writes for `{mcp_config_dir}`

[hooks]                        # optional table; how the launched CLI is made
                                # to report its turns. See "Hooks injection"
                                # below
# args = ["--settings", "{hooks_config_file}"]
# config_file = '...'          # optional; content of a settings file the
                                # daemon writes for `{hooks_config_file}`
# config_dir = { "hooks.json" = '...' }
                                # optional; files of a settings directory the
                                # daemon writes for `{hooks_config_dir}`

[credentials]                  # optional table; how the window's identity
                                # reaches the CLI's hooks and MCP bridge.
                                # See "Credentials" below
# mode = "launch"              # "env" (the default) or "launch"
# launch_args = ["-c", "..."]  # argv added to the spawn only

[delivery]                     # optional table; how the daemon hands the
                                # CLI the text it sends of its own accord.
                                # See "Delivery" below
# output = "plain"
# wake = "none"
# launch_context = ["--rules", "{context}"]
# launch_prompt = ["{prompt}"]

[host_paths]                   # optional table; how a path the daemon hands
                                # the CLI becomes the path the CLI's own
                                # environment sees, and how the CLI's own
                                # files are read there. See "Host paths" below
# convert = ["wsl", "-e", "wslpath", "-u", "{path}"]

[shim_guidance]                # optional table; what to tell a user when
                                # the program is a Windows .cmd / .bat file
                                # that something cannot pass. See "Shim
                                # guidance" below
# text = "Use the WSL profile for that."
```

The identifier is the file name without its `.toml` extension, and must
match `[a-z0-9_-]{1,64}`. A file whose name does not match this pattern, or
whose content fails validation, is left out of the listing entirely, with
one warning line naming it -- it never stops the rest of the directory from
being read.

A profile document fails validation outright (and is excluded) when: it
has a top-level key or a `[command]` key not listed here; `schema`
is not `1`; `display_name` is missing or violates the window-label rule; any
argv element under `[command]` is empty or contains a NUL byte; a
`[model]` / `[effort]` table breaks one of the rules under
[Launch options](#launch-options); an `[mcp]` or `[hooks]` table breaks
one of the rules under [MCP injection](#mcp-injection) and
[Hooks injection](#hooks-injection); a `[delivery]` table breaks a rule
under [Delivery](#delivery); a `[host_paths]` table breaks a rule under
[Host paths](#host-paths); or a `[shim_guidance]` table breaks a rule under
[Shim guidance](#shim-guidance). A profile stays valid, with the offending
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
resolved default shell (resolved once, when the daemon starts) --
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
injects into that window -- `YAGURA_WINDOW_ID` and `YAGURA_STATUS_TOKEN`
(unless the profile hands them over in a file; see
[Credentials](#credentials)), `YAGURA_MCP_BRIDGE/p` when a bridge was
resolved, and `YAGURA_INSTANCE` when the daemon itself was started with it --
so they also reach a process inside a WSL distribution. Items already in the value
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
variable the daemon itself inherited. The one exception is a window whose
profile hands its credentials over in a file (see
[Credentials](#credentials)): `YAGURA_WINDOW_ID` and `YAGURA_STATUS_TOKEN` are
taken out of its child's environment last, after `[env]`, whether the daemon
or the profile carried them.

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
currently be resolved to something that can be started at all** -- and, for a
profile that wakes its CLI by running its own program, whether that program
can be handed text (kind 5). It does not
guarantee that launching it will actually succeed -- the child process can
still fail to start or exit immediately for reasons resolution cannot see
(a missing subcommand, a bad working directory, the program itself failing).

An unavailable profile stays in the listing (so it is visible and its reason
is legible) rather than being hidden -- with one exception: a profile with no
argv at all for this operating system (kind 3 below) does not exist here at
all, and is left out of the listing entirely rather than shown as
unavailable (see [OS-specific `[command]` selection](#os-specific-command-selection)).
The reason is always one of five kinds, and never names a path or a user
name:

1. the executable could not be found
2. the command line contains a character the command interpreter would
   reinterpret (Windows `.cmd` / `.bat` shims only; see below)
3. no argv is defined for this operating system
4. no default shell was resolved when the daemon started (`[command]`
   omitted profiles only)
5. the profile wakes its CLI by running its own program with the text as an
   argument (`wake = "command"` in [`[delivery]`](#delivery)), and that
   program is a Windows `.cmd` / `.bat` file, through which the text cannot
   pass unchanged. The profile's own [`[shim_guidance]`](#shim-guidance)
   follows the fixed sentence as `text command (see url)`, and is also
   carried on its own beside the reason, in three parts, so a client can
   show them apart

An available profile can still be unable to carry one launch option or a
start-up request on this host; that is a separate fact, listed as the
profile's `limits` (see [Shim guidance](#shim-guidance)).

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

Text the daemon hands a CLI on its command line is the same kind of argument,
but it is arbitrary text, so it cannot be vetted for these characters ahead of
time. A request that must be passed that way -- a `launch_prompt`, or a wake
command's arguments (see [Delivery](#delivery)) -- is therefore never passed
through a shim: a window that needs a `launch_prompt` is refused, a
`launch_context` is left out of the launch, and a profile
that wakes its CLI by command is listed as unavailable (kind 5 under
[Availability](#availability)). A `[model]` or `[effort]` template that itself
holds one of those characters outside `{value}` cannot pass either, and is
refused the same way; both are listed beforehand (see
[Shim guidance](#shim-guidance)).

## Shim guidance

A profile's optional top-level `[shim_guidance]` table says what to do when
the profile's program is a Windows `.cmd` / `.bat` file and something cannot
pass through it. It is one table for every such case.

```toml
[shim_guidance]
text = "On Windows, this CLI's launcher cannot pass an effort level; use the WSL profile for that."
command = "irm https://example.com/install.ps1 | iex"   # optional
url = "https://example.com/docs"                        # optional
```

`text` is required and is one line of visible text; `command` (something to
copy and run) and `url` (an `https` address only) are optional and one line
each. Any other key, or a value outside these rules, fails the table.

When the program resolves to a `.cmd` / `.bat` file, the daemon lists what the
profile cannot do on this host, judged from the profile itself:

- `model` or `effort`, when an element of that option's `args` holds a
  character the command interpreter would act on outside `{value}`;
- `launch_prompt`, whenever the profile declares one, since the request is
  arbitrary text;
- `launch_context`, whenever the profile declares one, for the same reason.

Each is listed as one entry of the profile's `limits` -- a capability, the
reason, and the guidance above -- in
[`list_profiles`](../docs/mcp-setup.md#list_profiles) and in the GUI, before
any window is opened. A request that needs one of them is refused as
`unsupported` before its approval, and nothing is started; the message is
the reason, which ends with the guidance as `text command (see url)`. Those
requests are `open_window` with that `model` or `effort`, a `send_handoff`
that opens a window from the profile, and an `open_task_window` for a task
with an instruction. No request asks for `launch_context`: a window is opened
without it, so without the room's introduction, and the daemon logs one
warning. `limits` is not `available`: the profile stays
available, and a request that needs none of its limited capabilities opens a
window as usual. A profile whose wake command cannot pass is unavailable instead
(kind 5 under [Availability](#availability)), and its reason carries the same
guidance.

The bundled `muse-code` profile uses it to point at `wsl-muse-code`, and
`codex-cli` to give the command that installs Codex CLI's standalone build.

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

`args` is the argv this option appends, and `env` the environment variables
it sets for the launch. A table has `args`, `env` or both, and `{value}` must
appear at least once among the `args` elements and the `env` values together.
Every occurrence of `{value}`, in every element and every value, is replaced
by the resolved value -- always *inside* an element, so a value can never
add, split or reorder argv elements however it is written. `{value}` is the
only placeholder there is, and there is no escaping syntax.

```toml
# A CLI whose model flag would save the model as the user's own default
[model]
env = { GROK_DEFAULT_MODEL = "{value}" }
```

`env` is for a CLI that takes a value for one launch only through its
environment -- Grok Build's `-m` writes the model into the user's own
settings, `GROK_DEFAULT_MODEL` does not (Grok Build 1.0.70). Its rules:

- A key is a variable name: non-empty, without `=` or a NUL byte, not
  starting with `YAGURA_` and not `WSLENV` (both case-insensitively). Such a
  key excludes the profile instead of being dropped as it is under
  [`[env]`](#env), since dropping it would leave the chosen value silently
  without effect. `WSLENV` belongs to `[env]`: a WSL profile names a launch
  option's variable in its own `[env]` `WSLENV` for the variable to reach the
  CLI.
- `[model]` and `[effort]` may not both set a variable of the same name
  (case-insensitively), since which value took effect would be undecided.
- The variables are laid over the profile's `[env]` only when the option is
  asked for, a variable of the same name (case-insensitively) replaced. An
  option the caller left out sets nothing, and whatever `[env]` holds,
  including a value of your own for the same variable, applies.
- The value rule below admits no `"` and no `\`, so `{value}` can sit inside
  a JSON or TOML string in a value (`'{"model":"{value}"}'`) without ending
  the string.
- The variables travel in the window's environment, never on its command
  line, so a `.cmd` / `.bat` program limits none of them.
- A restored window reopened from its profile gets them again from the
  recorded value; one that falls back to its saved argv does not (see
  [Restoring a window opened from a profile](#restoring-a-window-opened-from-a-profile)).

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
listing, with one warning line naming the file: when both `args` and `env`
are missing, when neither contains `{value}`, when an `env` key breaks the
rules above, when an alias key or value breaks the
rules above, when a `[model.choices]` table breaks one of the rules under
[Model choices](#model-choices), or when the table carries any key other
than `args`, `env`, `aliases` and -- under `[model]` only -- `choices` (so a
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
as written on that path too. The profile's own `args` around `{value}` are
not covered by that rule; a template that needs such a character is listed as
a limit on that path (see [Shim guidance](#shim-guidance)).

On Windows, a value outside the launch option value rule is refused as
`invalid_argument` before any process starts, so it never reaches the
`.cmd` / `.bat` shim check described in
[`.cmd` / `.bat` shims](#cmd--bat-shims). A profile's listed `available`
flag says nothing about a launch option's value: it is judged against the
argv with **no** launch option applied, so "available" and "this particular
value cannot be launched" are both true at once.

### Recommended aliases

Every profile this repository ships declares `fast`, `default` and `strong`,
except the GitHub Copilot ones (see below).
Putting the same three names in a profile you write yourself lets whoever
asks for a window pick a tier without knowing which CLI, or which model
names, are behind it:

| profile | `model` aliases | `effort` |
|---|---|---|
| `claude-code`, `wsl-claude-code` | `fast` = `haiku`, `default` = `sonnet`, `strong` = `opus`, `fable` = `fable` | accepted, no aliases (levels pass through) |
| `codex-cli`, `wsl-codex-cli` | `fast` = `gpt-5.6-luna`, `default` = `gpt-5.6-terra`, `strong` = `gpt-6-astra` | accepted, no aliases (levels pass through) |
| `antigravity-cli`, `wsl-antigravity-cli` | `fast` = `gemini-3.8-flash-low`, `default` = `gemini-3.8-flash-medium`, `strong` = `gemini-3.1-pro-high` | not accepted (see below) |
| `copilot-cli`, `wsl-copilot-cli` | `default` = `auto` only (see below) | accepted, no aliases (levels pass through) |
| `muse-code`, `wsl-muse-code` | `fast` = `muse-spark-1.3`, `default` = `muse-spark-1.3`, `strong` = `muse-wonder-1.0` | accepted, no aliases (levels pass through) on macOS, Linux and in WSL; refused on Windows (see below) |
| `grok-build`, `wsl-grok-build` | `fast` = `grok-4.7-build-fast`, `default` = `grok-4.7`, `strong` = `grok-4.7` | accepted, no aliases (levels pass through) |
| `shell` | accepts neither option | -- |

Those mappings come from each CLI's own `--help` (Claude Code 2.1.263, Codex
CLI 0.153.4, Antigravity CLI 1.2.11, GitHub Copilot CLI 1.0.101, Muse Code
1.4.12, Grok Build 1.0.70) and, for the reasoning levels and the
model names, the Codex configuration and models reference and Muse's models
page. **Model names
change over time**: read the table as the state when these profiles were
written, and edit your own copies to whatever your CLI and your account
actually accept -- yagura never validates a model name, it only passes it
on. The `antigravity-cli` aliases were confirmed only against Antigravity's
free-tier "Starter Quota"; a model a paid tier can select was not checked.

GitHub Copilot CLI documents no model identifiers -- its supported models
page gives display names only -- and has no command that lists them, so the
Copilot profiles map `default` to `auto`, the value `--model` takes to let
Copilot pick, and nothing else. Pass any other model name as it is, or pick
one with the CLI's own `/model`. This was checked on a Copilot Free account.

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

The Muse profiles ship no `[effort]` table, so asking either for `effort` is
refused. Muse saves a level given with `--reasoning-effort` into the user's
own settings, and its one-run override `-c reasoning_effort="<level>"` is
ignored unless those settings already hold the key (Muse Code 1.4.12 to 1.4.14), so
there is no way to set it for one window (see
[Configuring Muse Code CLI](../docs/mcp-setup.md#configuring-muse-code-cli)).
Muse has no command that lists its models, so `[model.choices]` is a fixed
list.

The Grok Build profiles pass the model as `GROK_DEFAULT_MODEL` (see `env`
above) rather than with `-m`, which saves it as the user's own default, and
the level with `--reasoning-effort`, which is not saved. A level the chosen
model does not take is not refused: Grok Build falls back to the model's
default level without a word and starts as usual (Grok Build 1.0.70).

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
id and display name separated by a tab). `grok-build` and
`wsl-grok-build` carry a fixed `values` list, since `grok models` prints no
machine-readable form; once it prints JSON, the list can ask it instead.
`copilot-cli` and
`wsl-copilot-cli` carry none, since Copilot lists its models nowhere a
profile can read (see [Recommended aliases](#recommended-aliases)). Codex
CLI's `debug` command may change its output; if it does, edit the pointers
to follow it.

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

## Credentials

A window's hooks and MCP bridge must tell the daemon which window they belong
to. They do so with the window's id and its status token (`YAGURA_WINDOW_ID`,
`YAGURA_STATUS_TOKEN`). The optional `[credentials]` table says how they get
them:

```toml
[credentials]
mode = "launch"
```

- `mode = "env"` (the default, and what a profile without the table gets): both
  values are in the child's environment. Every process the CLI starts inherits
  them, so a CLI started by one of the window's tools reports as that window.
- `mode = "launch"`: the daemon writes a file for the window, readable by its
  owner only, holding the id and the token, and the profile's `[mcp]` and
  `[hooks]` hand its path to the hooks and the bridge as `--credentials <path>`
  (see `{credentials_file}` below). The child's environment holds neither
  value, and `WSLENV` does not forward them, so a CLI started by one of the
  window's tools cannot claim the window: its hook doors do nothing and its
  bridge connects without a window.
- `launch_args`: argv elements appended to the spawn only, after the profile's
  other injections. They are for a CLI that has to keep the credentials in its
  environment but can be told to withhold them from the tools it runs. They are
  not recorded with the window, so a restored window gets them again from the
  profile. Each element must be a non-empty string without a NUL.

A `[credentials]` table with another key, or a `mode` other than `"env"` and
`"launch"`, or `{credentials_file}` in a profile whose mode is not `"launch"`,
excludes the profile with a warning.

**`{credentials_file}`** is the absolute path of the window's credentials file
(with `/` separators on Windows). It can be written in the `args`,
`config_file` and `config_dir` contents of `[mcp]` and `[hooks]`. Inside a
hook command, which a shell reads, write it in single quotes
(`yagura-daemon --credentials '{credentials_file}' --emit-status ...`); in a
JSON or TOML string it needs no escaping. A window's settings files are written
in a folder of that window's own, next to its credentials file, instead of the
shared `mcp/` folder, because their content names the path.

The file lives as long as the window's child: it is removed when the child
exits, and what an earlier run left behind is removed when the daemon starts.
The window still opens, without its hooks and its MCP injection and with a
warning in the daemon log, when the file or its folder cannot be made, or when
the location holds a `'`, a `"`, a `\` or a control character. A window whose
profile asks for `"launch"` but whose hooks or bridge do not all spell
`{credentials_file}` opens with its credentials in the environment instead, so
that they do not go silent.

**How a door or the bridge decides which window it belongs to.** In order: both
environment variables, when neither is empty; otherwise the file named by a
single `--credentials <path>`; otherwise it is outside any window. An empty
value counts as unset, because a CLI can only spell "unset" as empty. A file
that cannot be read, is larger than 4 KiB, is not the expected JSON object
(`window_id` and `token`) or has an empty field names no window either.

**The bundled profiles.** `claude-code`, `copilot-cli` and `wsl-muse-code`,
and the WSL variants of the first two, use `"launch"`. `codex-cli` and
`wsl-codex-cli` stay on `"env"`: Codex runs a hook only while the hash of its
command is the one it has recorded as trusted, so the command cannot carry a
per-window path. Their `launch_args` set both variables to the empty string in
`shell_environment_policy`, which keeps the real values out of the environment
of the tools Codex runs. `grok-build`, `antigravity-cli`, `muse-code` and
`shell` (and the WSL variants of the first two) keep `"env"` too: their hooks are
written into the CLI's own settings by a registration, once for all windows,
which has no place for a per-window path.

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
`muse-code` has none either: Muse Code's Windows launcher cannot carry the
JSON a launch would pass, so on every operating system that profile reaches
yagura through its registration instead (see
[MCP registration](#mcp-registration)). Nor do `grok-build` and
`wsl-grok-build`: Grok Build takes MCP servers only from the settings files
it reads, with no launch argument that adds one (see
[Configuring Grok Build](../docs/mcp-setup.md#configuring-grok-build)).

```toml
# Claude Code on Windows: the server definition goes into a settings file
[mcp]
config_file = '{"mcpServers":{"{server}":{"command":"${YAGURA_MCP_BRIDGE}","args":[]}}}'
args = ["--mcp-config", "{mcp_config_file}"]

# Codex CLI on Windows: -c overrides written as TOML literal strings
[mcp]
args = [
  "-c", "mcp_servers.{server}.command={bridge_toml}",
  "-c", "mcp_servers.{server}.args=[]",
  "-c", "mcp_servers.{server}.env_vars={env_names_toml}",
  "-c", "mcp_servers.{server}.tool_timeout_sec=86400",
]
```

The table takes four keys, and any other key excludes the profile:

- `args` (required): argv elements appended to the launch, after the
  profile's own argv and its launch options.
- `config_file` (optional): the content of a settings file for the CLI to
  read, written by the daemon as given except that `{server}` is replaced
  by the server name (see the table below) -- no other placeholder is
  substituted inside it, so JSON braces and `${NAME}` stay as written.
- `config_dir` (optional): a table of file name to content, for a CLI that
  takes its settings only as a directory of files. The daemon writes a
  directory holding exactly these files, each content treated as
  `config_file`'s is. A file name is one plain name: ASCII letters, digits,
  `.`, `_` and `-` only, not starting or ending with `.`, without `..`, and
  not a Windows device name (`con`, `nul`, `com1` and so on). `config_file`
  and `config_dir` can be used together.
- `absent` (optional): `{ file = "<home-relative path>", pointer = "<JSON
  Pointer>" }`, an entry of the CLI's own JSON settings that, when it is
  there, takes the injection out of the launch. For a CLI that refuses to
  start when the user's own settings already define a server of the injected
  name (`pointer` may carry `{server}`). The file is read before each launch
  -- through [`[host_paths]`](#host-paths)'s `read` when the profile has one,
  directly otherwise -- within the same slots and 10-second limit as a path
  conversion. When the entry is there, or the file cannot be read in time,
  the window opens without the `[mcp]` injection and the daemon logs one
  warning; a missing file or entry keeps the injection. `wsl-muse-code`
  uses it.

```toml
# A CLI that reads its MCP servers from a directory
[mcp]
args = ["--config-dir", "{mcp_config_dir}"]

[mcp.config_dir]
"mcp.json" = '{"mcpServers":{"{server}":{"command":"${YAGURA_MCP_BRIDGE}","args":[]}}}'
```

These placeholders are substituted inside `args` elements, always within an
element, so they can never add, split or reorder argv elements:

| placeholder | replaced by |
|---|---|
| `{server}` | the MCP server name the instance registers under: `yagura` for the default instance, `yagura-<instance>` for a named one (see [Instances](../docs/instances.md)); also substituted inside `config_file` and each `config_dir` file |
| `{bridge_toml}` | the bridge's absolute path as a TOML literal string, for example `'C:\path\to\yagura-mcp.exe'` |
| `{env_names_toml}` | the TOML literal-string array `['YAGURA_WINDOW_ID', 'YAGURA_STATUS_TOKEN', 'YAGURA_INSTANCE', 'YAGURA_MCP_BRIDGE']` |
| `{wslenv_toml}` | the TOML literal string `'YAGURA_WINDOW_ID:YAGURA_STATUS_TOKEN:YAGURA_INSTANCE'`, a `WSLENV` value naming the variables the bridge reads (the bundled `wsl-codex-cli` sets it as its server's `env.WSLENV`, because Codex does not pass `WSLENV` on to an MCP server, and without it the Windows bridge started from WSL sees none of them) |
| `{bridge_json}` | the bridge's absolute path as a JSON string, for a CLI that takes its server definition as JSON on the command line |
| `{env_refs}` | a JSON object naming, each as a reference to itself (`{"YAGURA_WINDOW_ID":"${YAGURA_WINDOW_ID}",...}`), the variables among `YAGURA_WINDOW_ID`, `YAGURA_STATUS_TOKEN`, `YAGURA_INSTANCE` and `WSLENV` that this window's child actually has; built when the window opens, so a variable the child lacks is never named. For a CLI that passes an MCP server only the variables its definition names and refuses a reference to one that is unset. Values never appear, only names |
| `{credentials_file}` | the path of the window's credentials file, for the hooks and the bridge to read the window's identity from; usable only in a profile whose `[credentials]` mode is `"launch"` (see [Credentials](#credentials)) |
| `{mcp_config_file}` | the path of the file the daemon wrote from `config_file`; usable only when the table has `config_file` |
| `{mcp_config_dir}` | the path of the directory the daemon wrote from `config_dir`; usable only when the table has `config_dir` |

An element with none of them passes through unchanged. `${NAME}` is not a
placeholder: it is left for the CLI to expand from its own environment,
which is how the bundled Claude Code tables name the bridge
(`${YAGURA_MCP_BRIDGE}`). The whole profile is excluded, with one warning
line naming the file, when `args` is missing, when an element carries any
other `{name}`-shaped token (ASCII letters, digits and `_` between braces),
when `{mcp_config_file}` is used without `config_file` or `{mcp_config_dir}`
without `config_dir`, or when `config_dir` is empty or has a key that is not
a plain file name.

What the daemon provides around the table:

- **The bridge.** At start-up the daemon looks for `yagura-mcp` next to its
  own executable (the installer ships both side by side). When there is none
  usable, it logs a warning and no window gets an injection.
- **The environment.** Every window's child gets `YAGURA_MCP_BRIDGE`, the
  bridge's absolute path, next to `YAGURA_WINDOW_ID` and
  `YAGURA_STATUS_TOKEN` (the last two unless the profile's credentials mode is
  `"launch"`). The token never appears in the injected argv or in a settings
  file: it is in the environment or in the window's credentials file.
- **The settings file.** `config_file` is written under the instance's
  state directory, in `mcp/`, as `mcp-<SHA-256 of the content>.json`. A file
  already there under that name is not written again.
- **The directory of settings files.** `config_dir` is written in the same `mcp/`
  folder as `mcp-<SHA-256>`, hashed over every file name and content in name
  order (no extension, so it never meets a settings file's name). The files
  are written into a temporary directory first and it is then renamed into
  place, so a CLI never sees a half-written directory. A directory already
  there under that name is not written again.

The injection applies to every launch from a profile -- `open_window` with
`profile`, a `send_handoff` that opens a new window from a `profile`,
`open_task_window`, and the GUI's add-window form with a profile selected --
and to none from a raw argv. It is built again for each launch and is not
recorded with the window, so a saved session never carries it.

The window still opens, without the injection and with a warning in the
daemon log, when the injection cannot be carried out:

- the bridge's path contains a `'` or a newline, which a TOML literal string
  cannot hold;
- the table has `config_file` or `config_dir` but no settings directory was
  resolved, or either of them cannot be written (the whole table is dropped);
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
straight on the command line. The Copilot profiles follow the same split:
`copilot-cli` names its settings file with Copilot's `@` prefix, and
`wsl-copilot-cli` passes the JSON inline. `wsl-muse-code` passes its JSON
inline too, with `{bridge_json}` and `{env_refs}` in it.

Where a CLI already has a server of the injected name configured -- `yagura`,
or `yagura-<instance>` for a named instance, the name Register in the GUI
uses -- the bundled tables take over for a window yagura opens. Claude Code
starts the `--mcp-config` definition instead of a same-named one from a
project's `.mcp.json`. Codex CLI merges `-c` overrides key by key, so keys
of an existing `[mcp_servers.<name>]` table that the injection does not set
stay in effect. A profile of your own that spells `yagura` instead of
`{server}` still works, but under a named instance its window then carries
two servers, the registered one and the injected one. The bundled Codex tables set `tool_timeout_sec` to a day: Codex's
own 60-second default would otherwise end a `wait_for_status`,
`wait_for_report` or `send_handoff` call that is still waiting on the daemon.
Copilot CLI lays `--additional-mcp-config` over its own configuration, the
last definition of a name winning. The bundled Copilot tables set `timeout`
to a day, in milliseconds: Copilot's own 180-second default would end the
same calls. Muse Code refuses to start when a `-c` definition names a server
its settings already define, which is why `wsl-muse-code` has `absent`; and
its `-c` takes no `tool_timeout_sec`, which is why that table sets none (see
[Configuring Muse Code CLI](../docs/mcp-setup.md#configuring-muse-code-cli)).

## Hooks injection

A profile's optional `[hooks]` table hands the launched CLI the hooks that
make it report its turn boundaries to the daemon with
`yagura-daemon --emit-status` (what the reports do is in
[Structured sources](../crates/yagura-daemon/detection/README.md#structured-sources---emit-status)).
The same hooks carry the doors a CLI reads its inbox through,
`yagura-daemon --deliver-context`, `--deliver-take`, `--deliver-wait` and
`--report-session` (see [Delivery](#delivery)). A hook can instead call
`yagura-daemon --hook <profile-id> <Event>`, which does whatever the
profile's [`[hook_events]`](#hook-events) table says for that event.
It follows the `[mcp]` rules above -- the same `args`, `config_file` and
`config_dir` keys, the same settings directory, argv order and drop rules --
with three differences:

- The placeholders are `{hooks_config_file}`, the path of the file written
  from `config_file` (usable only when the table has `config_file`), and
  `{hooks_config_dir}`, the path of the directory written from `config_dir`
  (usable only when the table has `config_dir`). The file is named
  `hooks-<SHA-256 of the content>.json` and the directory `hooks-<SHA-256>`.
  Nothing is substituted inside either: `{server}` belongs to `[mcp]`.
- It needs no bridge: a profile's hooks are injected whether or not
  `yagura-mcp` was found.
- In a profile whose credentials mode is `"launch"`, each door's command
  carries `--credentials '{credentials_file}'` (see [Credentials](#credentials)).
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
  "-c", "hooks.Stop=[{hooks=[{type='command',command='yagura-daemon --hook codex-cli Stop'}]}]",
]

[hook_events]
Stop = { status = "waiting_input" }

# A CLI that takes its hooks only as a directory of files
[hooks]
args = ["--plugin-dir", "{hooks_config_dir}"]

[hooks.config_dir]
"plugin.json" = '{"name":"yagura-status"}'
"hooks.json" = '{"hooks":{"Stop":[{"type":"command","command":"yagura-daemon --emit-status waiting_input --source my-cli"}]}}'
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
one is in place, after writing out-of-date ones again while the instance is
still registered (see "Out-of-date entries" under
[MCP registration](#mcp-registration)). This is how the bundled Codex profiles work: Codex runs a
hook given with `-c` only once its trust is recorded in `~/.codex/config.toml`,
and the key that records it carries characters a shim refuses, so
registration writes the trust and the launch injects the hooks. A profile with
no `[hooks]` table but a `json_entry` -- Antigravity CLI and Grok Build, which
read hooks only from their own files, and `muse-code`, whose hooks live in
the managed hooks file its registration writes -- injects nothing; the entry
is the hook.

**Which windows expect reports.** A window whose `[hooks]` elements were
actually appended to its launch, and a window whose profile's `json_entry` was
in place when it opened, expect their CLI to report. Such a window is marked
in the GUI when no report arrives (see
[When reports are missing](../crates/yagura-daemon/detection/README.md#when-reports-are-missing)).
A table that was dropped on the way -- no settings directory, settings that
could not be written, a shim, entries not in place -- expects nothing, and
its window takes no part in [delivery](#delivery) either.

The bundled profiles map these events (the versions are those the profiles
were checked with):

| Profiles | `running` | `waiting_approval` | `waiting_input` |
| --- | --- | --- | --- |
| `claude-code`, `wsl-claude-code` (Claude Code 2.1.285) | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `PermissionRequest` | `Stop`, `StopFailure` |
| `codex-cli` (Codex CLI 0.159.2), `wsl-codex-cli` (0.158.0) | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `PermissionRequest` | `Stop`, `Interrupt` |
| `antigravity-cli`, `wsl-antigravity-cli` (Antigravity CLI 1.2.14, through `json_entry`) | `PreInvocation`, `PostToolUse` | -- | -- |
| `copilot-cli`, `wsl-copilot-cli` (GitHub Copilot CLI 1.0.101) | `userPromptSubmitted`, `preToolUse`, `postToolUse`, `postToolUseFailure` | -- | `agentStop` |
| `muse-code` (through `json_entry`), `wsl-muse-code` (Muse Code 1.4.12) | `UserPromptSubmit` | `PermissionRequest` | `Stop`, `StopFailure`, `Interrupt` |
| `grok-build`, `wsl-grok-build` (Grok Build 1.0.70, through `json_entry`) | `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | `Notification` (`permission_prompt`) | `Stop`, `StopFailure`, `StopCancelled` |

The bundled profiles also call the delivery doors from the hooks above: at
session start, at the start of a turn and, for Claude Code, in a background
wait. The hooks of the Antigravity CLI, Grok Build, `muse-code` and Codex CLI
profiles call `--hook` for every event instead, and their `[hook_events]`
tables say which event reports, delivers or skips (see
[Hook events](#hook-events)). Grok Build hands the model a hook's output only
from the tool-call hooks, so the Grok Build entry takes the inbox after each tool call
(`PostToolUse`, `PostToolUseFailure`) and has no session-start door; the
room's introduction goes in at launch instead (`launch_context`, see
[Delivery](#delivery)).

Grok Build also runs these hooks in a sub-agent's session, whose input
carries `subagentType`, so each of its events has `skip_if = "/subagentType"`.
The other doors take such a pointer as `--skip-if <JSON Pointer>` (starting
with `/`). Either way, when the JSON the CLI writes to the hook's standard
input has a value other than `null` there, the door does nothing and exits
0. The pointer is the profile's data; the daemon knows no CLI's field. Every door reads its
standard input to the end, so the CLI's write to it does not fail; inside a
window, a door that has answered waits at most 2 seconds more for a CLI that
keeps its standard input open.

Copilot CLI takes launch-time hooks only from a plugin directory
(`--plugin-dir`), so the Copilot profiles write one with `config_dir`: a
`plugin.json` that names the plugin and a `hooks.json` beside it. Copilot
loads it once the folder is trusted and runs `sessionStart` -- the
start-up context door -- when the first prompt is sent, not when the window
opens. An interrupted turn runs no hook, and an approval prompt is read
from the screen only.

`wsl-claude-code` passes the JSON on the command line, since the Windows path
of a settings file is unreadable from inside WSL. Changing a Codex hook --
its command or another of its attributes -- changes the hash Codex trusts it
by, so the matching `toml_set.entries` value has to be taken again (see
[MCP registration](#mcp-registration)). A `--hook` command stays the same
from release to release, so only a change of the other attributes, or of the
profile's id, calls for that.

### Hooks that run outside a window

A hook a registration writes into the user's own settings runs in every
session of that CLI, inside a window or not. Outside a window the `bin`
folder is not on `PATH`, so a bare `yagura-daemon` is not found and the CLI
reports a failed hook on every turn. The bundled `muse-code` hooks therefore
check for the window in the shell before they start the daemon, and do
nothing outside one:

| Host | Command form (`%COMSPEC% /C` on Windows, `$SHELL -c` elsewhere) |
| --- | --- |
| Windows (`by_os.windows`) | `if defined YAGURA_WINDOW_ID (set NoDefaultCurrentDirectoryInExePath=1& yagura-daemon --hook muse-code UserPromptSubmit)` |
| macOS, Linux (`by_os.default`) | `[ -z "$YAGURA_WINDOW_ID" ] \|\| exec yagura-daemon --hook muse-code UserPromptSubmit` |

The check only tests whether the variable is set and non-empty; its value is not expanded
into the command. On Windows the same command also sets
`NoDefaultCurrentDirectoryInExePath` before `yagura-daemon` is looked up, so
`cmd` does not run a `yagura-daemon.bat` or `.exe` from the session's working
folder ahead of the one on `PATH`; it applies whether yagura or the user
started that Muse. The door itself still does nothing outside a
window, which covers a hook written by hand without the check. The
`wsl-muse-code` hooks file is passed with each launch and read only by that
window's CLI, so its commands carry no check.

A CLI whose hook can name a program and its arguments apart, with no shell
in between, takes a second form: the hook runs `{daemon}`, the absolute path
of the daemon's copy, directly. The bundled Grok Build hooks are written this
way, since Grok Build runs a hook with `args` as one program and starts no
shell, while the shell it would otherwise use differs from machine to
machine:

```toml
UserPromptSubmit = [{ hooks = [{ type = "command", command = "{daemon}", args = ["--for-instance", "{server}", "--hook", "grok-build", "UserPromptSubmit"], timeout = 10 }] }]
```

Nothing in the hook checks for the window: the door does. Outside a window
-- neither the environment pair nor a `--credentials` file naming a window --
a door does not interpret its other arguments at all: it reads its standard input to the end
and exits 0 without printing anything. So a copy of the daemon older than the
hook, which does not know an argument such as `--hook`, stays silent there
too. Inside a window, a copy that cannot read the command line also reads its
standard input to the end and exits 0, logging a warning that names the flag
it did not know; a copy from 0.3.0 or earlier refuses it there instead (see
[Structured sources](../crates/yagura-daemon/detection/README.md#structured-sources---emit-status)). The path needs no `PATH` lookup and no search of the working folder,
and under WSL `[host_paths]` turns it into the `/mnt/...` path the
distribution starts the Windows executable from. The same rule as for any
`{daemon}` applies: a path holding whitespace is written by its 8.3 short
name on Windows, and is otherwise not written at all.

### Hooks of other instances

A hook that registration writes into the CLI's own settings runs in every
session of that CLI, once for each instance registered there -- including
inside another instance's window, where it would reach that window's daemon.
So a registration that writes hooks of its own for each instance (an entry
keyed by `{server}`, or a file named by it) passes `--for-instance {server}`
to every door it runs, right after `{daemon}`, as the bundled Antigravity CLI
and Grok Build hooks do. Inside a window a door whose `--for-instance` is not
the window's own instance does nothing, like a door outside a window (see
[Structured sources](../crates/yagura-daemon/detection/README.md#structured-sources---emit-status)).
Hooks passed with a launch, or one file all instances share, run only once
and need no `--for-instance`.

## Hook events

A profile's optional `[hook_events]` table says what the daemon does when a
hook calls `yagura-daemon --hook <profile-id> <Event>` with the profile's id.
The table is data: the daemon never branches on which CLI a profile is for.

```toml
[hook_events]
PreInvocation = { status = "running", deliver = "take" }
SessionStart = { deliver = "context" }
PostToolUse = { status = "running", skip_if = "/subagentType" }
UserPromptSubmit = { status = "running", session = "/session_id" }
```

Each key is an event name, matching `[A-Za-z0-9_-]{1,64}`: the name the
hook passes after the profile's id, usually the CLI's own name for the event.
Each value is a table of these keys:

| Key | Meaning |
| --- | --- |
| `status` | The status to report for the window, one of the seven statuses, as `--emit-status` reports it (see [What a report means](../crates/yagura-daemon/detection/README.md#what-a-report-means)) |
| `deliver` | `"context"`: print the room's introduction and what waits in the inbox, as `--deliver-context` does. `"take"`: print what waits and mark it taken, as `--deliver-take` does |
| `session` | A JSON Pointer to the CLI's session identifier in the hook's input, recorded as `--report-session` records it |
| `skip_if` | A JSON Pointer: when the hook's input has a value other than `null` there, the event does nothing |

They run in a fixed order: `skip_if`, then `session`, then `status`, then
`deliver`. An event needs at least one of `status`, `deliver` and `session`.
A pointer starts with `/` and has at most 256 characters. An unknown key, or
a value of the wrong type or outside its domain, fails the table, and the
profile is left out of the listing with a warning.

**Which profile is read.** The daemon reads the profile the hook names --
the shipped one with your own file laid over it, like any other read -- each
time the hook runs, not the profile the window was opened from. So the hook
works in a window opened from another profile or from a raw command, and a
change to your file applies from the next hook on. Text is printed in that
profile's [`[delivery]`](#delivery) `output` form (its `context` or `take`
template) and within its `max_chars`; a profile without `[delivery]` prints
in the window's own form. A profile the daemon cannot read or would not
list, or one with no such event, does nothing: the hook prints nothing,
exits 0, and the daemon logs a warning.

**What leaves the hook.** The door reads the hook's input itself and sends
the daemon only what the event asks for: whether `skip_if` holds a value, and
the session identifier when it has the shape of one. Nothing else of the
input reaches the daemon. Like the other doors, `--hook` does nothing
outside a window and takes `--for-instance` and `--credentials`; it takes no
`--skip-if`, since the pointers are the profile's.

**When to use it.** A hook that stays in the CLI's own settings after a
window ends -- written there by a registration, or trusted by a hash the CLI
records over its command -- calls `--hook`, so its text does not change from
release to release and a later release can change what it does without
anyone writing the entry again. The bundled Antigravity CLI, Grok Build,
`muse-code` and Codex CLI profiles do this. Hooks passed with every launch
may call the other doors directly.

## Delivery

A profile's optional `[delivery]` table says how the daemon hands the profile's CLI the text it sends of its own accord -- chat, handoffs and notices -- without typing it into the composer. The mechanism (the inbox, the doors a CLI's hooks run, and when each text arrives) is described once, in [How the daemon delivers text](../docs/mcp-setup.md#how-the-daemon-delivers-text); this section covers the table.

The table is data: the daemon reads it and never branches on which CLI a profile is for. A window is handed text this way only if its profile declares `[delivery]` **and** the hooks that call the doors were put in place when it opened (`[hooks]`, or a `json_entry` for a CLI that reads hooks only from its own file; see [Hooks injection](#hooks-injection)). A window that is not -- a profile with no `[delivery]` table, a raw `command`, a hook table that was dropped on the way -- is no chat participant, gets no announcement, and a handoff addressed to it is answered `undelivered_no_delivery`.

```toml
[delivery]
output = "plain"                # "plain", or a JSON template containing {text}
max_chars = 10000               # the most characters one answer carries
wake = "wait"                   # "none" (default), "wait" or "command"
wake_stream = "stderr"          # "stderr" (default) or "stdout"
wake_exit = 2                   # exit code of an answered wait (default 2)
launch_context = ["--rules", "{context}"]
launch_prompt = ["--", "{prompt}"]

[delivery.output]               # or this table instead of the one string
context = '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":{text}}}'
take = '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":{text}}}'
```

| Key | Meaning |
| --- | --- |
| `output` | How a door prints the text. `"plain"` (the default) prints it as it is. Anything else is a JSON template that contains `{text}`: the text is inserted as a JSON string (quoted and escaped), and the template must still be valid JSON with it in. A CLI that wants its hook output wrapped in a structure takes this form. One string applies to every door. A table `[delivery.output]` gives each door its own, keyed `context` (`--deliver-context`, and a [hook event](#hook-events) with `deliver = "context"`), `take` (`--deliver-take`, and `deliver = "take"`) and `wait` (`--deliver-wait`), each value following the same rule; a door the table leaves out prints plain, and any other key fails the table. A CLI that checks a hook's output against the event the hook ran for takes this form |
| `max_chars` | The most characters one answer carries, the chat announcement included (default 10000). Texts are never cut; the ones that do not fit wait for the next answer. Where the CLI limits a hook's output itself, set this to that limit |
| `wake` | How a CLI that is waiting for input is woken. `"none"` (the default): the daemon cannot, so it types the oldest waiting text into the composer while the window waits for input (this needs a composer and an `[input].probe` in the window's detection definition, see [Detection definitions](../crates/yagura-daemon/detection/README.md#composer)). `"wait"`: the CLI leaves a wait registered through the `--deliver-wait` door and the daemon answers it. `"command"`: the daemon runs the profile's own program with `wake_args` |
| `wake_args` | Required with `wake = "command"` and rejected otherwise. The arguments the program is run with; each is a non-empty argv element, at least one contains `{text}`, and `{session}` stands for the session identifier a hook reported with `--report-session` (or a [hook event](#hook-events)'s `session`). The command runs once the CLI has reported its session, so not before its first turn |
| `wake_stream`, `wake_exit` | For `wake = "wait"`: the stream an answered wait prints on and the exit code it ends with. The CLI's own hook documentation says which code wakes it |
| `launch_prompt` | Arguments that hand the CLI a first request when it starts; `{prompt}` (required in at least one element) is replaced with the text. The elements are appended after the launch's other elements, after `[mcp]` and `[hooks]`. A task window with an instruction, and a window a `send_handoff` opens from this profile, are started this way. A profile without it cannot be used for either (`no_composer`) |
| `launch_context` | Arguments that hand the CLI the room's introduction (the playbook's `[chat] intro`) when it starts, for a CLI with no session-start hook whose output reaches its model; `{context}` (required in at least one element) is replaced with the text. They are added only for a window that will take part in chat -- the profile's hooks are in place and its detection definition is not in `[chat] exclude_detections` -- and come after the launch's other elements, ahead of the `launch_prompt` elements. When the program is a Windows `.cmd` / `.bat` file they are left out, and the daemon logs one warning (see [Shim guidance](#shim-guidance)) |

`wake_timeout_secs`, `session_pointer` and `shim_guidance` are not keys: writing any of them is an error (what to tell a user when the program is a `.cmd` / `.bat` file is the top-level [`[shim_guidance]`](#shim-guidance) table). An unknown key, a value of the wrong type or outside its domain, or a `{text}` / `{prompt}` / `{session}` / `{context}` placeholder in a place that does not take it fails the table, and the profile is left out of the listing with a warning, like any other invalid table.

**Where each bundled profile declares what.** The tables are in `claude-code.toml`, `codex-cli.toml`, `antigravity-cli.toml`, `copilot-cli.toml`, `muse-code.toml`, `grok-build.toml` and their `wsl-` counterparts; read them as the working examples. In short: Claude Code leaves a wait that ends with exit code 2 (`wake = "wait"`), Codex CLI is woken with its own `queue` subcommand (`wake = "command"`), and Antigravity CLI, GitHub Copilot CLI, Muse Code and Grok Build have no way to be woken (`wake = "none"`), so while one of their windows waits for input the daemon types the text. Muse Code takes a hook's output for JSON once it starts with `[`, as yagura's texts can (`[yagura handoff] ...`), and a JSON answer must name the event it is for, so its profiles give `context` and `take` their own templates. Grok Build passes no session-start hook's output to its model, so its profiles hand it the room's introduction with `--rules` (`launch_context`), and take the inbox only after a tool call. Their start-up requests are given on each CLI's command line.

**Windows and Codex CLI.** A wake command passes the text as a command-line argument, and a Windows `.cmd` / `.bat` file is started through the command interpreter, which reads parts of an argument as its own syntax. Such a profile is therefore listed as unavailable (reason kind 5 under [Availability](#availability)) instead of being started and failing later. For Codex CLI, whose npm package installs a `.cmd` shim, install the standalone build and put it ahead of the npm one on `PATH`; the bundled `codex-cli` profile's [`[shim_guidance]`](#shim-guidance) gives the command. A `launch_prompt` is subject to the same limit: a request that must be passed on the command line is refused when the program is a `.cmd` / `.bat` file (see [Shim guidance](#shim-guidance)).

To add a profile for a new CLI, work out which doors and wake it supports first; the procedure is in [Adding a CLI](../docs/adding-a-cli.md).

## Host paths

A CLI that runs in another environment than the daemon -- a WSL
distribution started from Windows -- cannot read the daemon's own paths as
they are. A profile's optional top-level `[host_paths]` table names the
command that turns one of them into the path that environment sees, and
every path the daemon hands the CLI goes through it: the settings file and
directory an injection names (`{mcp_config_file}`, `{mcp_config_dir}`,
`{hooks_config_file}`, `{hooks_config_dir}`), the bridge `{bridge_toml}`
names, and, for [MCP registration](#mcp-registration), the bridge `add`
registers (`{bridge}`) and the daemon a `json_entry` names (`{daemon}`).
The bridge `{bridge_json}` names is converted the same way.
Without the table, paths are handed over unchanged.

```toml
[host_paths]
convert = ["wsl", "-e", "wslpath", "-u", "{path}"]
read = ["wsl", "-e", "sh", "-c", 'f="$HOME/$1"; [ -e "$f" ] || exit 3; exec cat -- "$f"', "sh", "{file}"]
absent_exit = 3
```

- `convert` is required: a non-empty argv, at least one element of which
  contains `{path}`, the daemon-side path. No other placeholder is accepted
  in it, and no key other than `convert`, `read`, `replace` and
  `absent_exit` (below) is accepted in the table; either fails the table, and
  the profile is left out of the listing with a warning.
- It runs as its own standalone command -- not after the profile's launch
  argv -- with the profile's `[env]`, in the daemon's empty folder for
  registration commands (see [MCP registration](#mcp-registration)). Its standard
  output must be exactly one line that starts with `/`, holds no control
  character and is at most 4 KiB; anything else counts as a failure.
- At a window's launch it runs only when an injection names one of the paths
  above, once per path, before the argv is put together. The wait for a free
  slot and the conversions together are limited to 10 seconds, the same
  slots and limit [the registration listing](#mcp-registration) uses. When a
  conversion fails or times out, the daemon logs a warning and opens the
  window without the table that names the path: a window left without its
  `[hooks]` this way is not expected to report its status and takes no part
  in [Delivery](#delivery). Nothing is cached; the next launch converts
  again.
- For a registration row it runs before `add` and before a state read, under
  the registration's own time limit. The converted bridge is compared with
  the CLI's record exactly, on every host, because the conversion already
  spelled it the way that environment does; a conversion that fails leaves
  the row **not installed** or **unknown**.

The table can also say how the CLI's own settings files are read and written
in that environment, for the profile's registration and its `[mcp]` `absent`
guard:

- `read` (optional): an argv that prints one file. `{file}` stands for the
  file's path relative to that environment's home directory and is the only
  placeholder; it is handed to the tool as its own argument (a positional
  argument to `sh` above), never spliced into a script string.
- `replace` (optional, needs `read`): an argv that replaces the file with
  what it is given on standard input, with the same `{file}`.
- `absent_exit` (optional, needs `read`): the exit code `read` uses for "the
  file is not there", told apart from every other failure.

They run as standalone commands, like `convert`. Without `read`, files are
read and written on the host directly. A registration's own
[`[mcp_registration.files]`](#mcp-registration) table holds the same three
keys and is still accepted; a profile may have one or the other, and having
both `[mcp_registration.files]` and `[host_paths]`'s `read` fails the
registration table, so the CLI's files are reached one way only. The bundled
WSL profiles that read the CLI's files use `[host_paths]`.

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
                                # add and remove: both, or neither (see
                                # "Registering without a command" below)
remove = ["mcp", "remove", "{server}"]
env_arg = ["--env={env}"]      # optional; required whenever a named
                                # instance's environment has to reach `add`
env_forward = { name = "WSLENV", separator = ":" }
                                # optional; names the variable a named
                                # instance's environment must also be listed
                                # in for `add`'s pairs to reach the process
notice = "While this is registered, ..."
                                # optional; one line the GUI shows on the row

[mcp_registration.status]      # required; how a row's current state is read
args = ["mcp", "get", "{server}", "--json"]  # exactly one of args / file
format = "json"                 # "json" or "lines"
command = "/transport/command"  # JSON Pointer ("json") or line prefix ("lines")
args_pointer = "/transport/args"  # optional; "json" only
# items = ""                    # optional, "json" only, together with `name`:
# name = "/name"                # the array of servers, and the pointer to an
                                # element's name; see "Reading a row's state"
require = { "Scope:" = "User" }   # optional; "lines" only
absent = "No MCP server named"    # optional; a substring meaning "not registered"

[mcp_registration.list]        # optional; turns the other instances' bridge
                                # registrations off for each window
args = ["mcp", "list", "--json"]  # appended to the launch argv; prints JSON
items = ""                      # JSON Pointer to the array of registered
                                # servers, or to a table keyed by their names
name = "/name"                  # JSON Pointer, within an element, to its
                                # name; omitted when `items` is a table, whose
                                # keys are the names
command = "/transport/command"  # JSON Pointer, within an element, to the
                                # executable it runs
disable = ["-c", "mcp_servers.{server}.enabled=false"]
                                # appended to the window's command line once
                                # per name; `{server}` only

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
[mcp_registration.toml_set.entries.retired]
                                # optional; keys under `under` an earlier
                                # version wrote, taken out by Register,
                                # Update and Remove. Lists chosen like the sets
windows = ['C:\<session-flags>\config.toml:stop:0:1']
default = ['/<session-flags>/config.toml:stop:0:1']

[mcp_registration.json_entry]  # optional; entries of a JSON settings file
                                # the registration owns. One table, or an
                                # array [[mcp_registration.json_entry]] of
                                # one per file
file = ".gemini/config/hooks.json"  # home-relative; may carry {server}
# claim = "/managed_hooks_path"  # optional; see "Shared entries and the claim"
[mcp_registration.json_entry.entries."{server}"]
                                # a top-level name, or a JSON Pointer such as
                                # "/mcpServers/{server}"
PreInvocation = [{ type = "command", command = "{daemon} --for-instance {server} --hook antigravity-cli PreInvocation", timeout = 10 }]
# [mcp_registration.json_entry.by_os.windows]
                                # optional; more entries for one OS: windows /
                                # macos / linux override default

[mcp_registration.files]       # optional; how a home-relative status.file,
                                # toml_set.file or json_entry.file is read and
                                # written inside another environment (WSL)
                                # rather than directly. The same keys as
                                # [host_paths]'s read / replace / absent_exit,
                                # which a profile has instead
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
path to it once the profile's [`[host_paths]`](#host-paths) converts it),
`{env_args}` (only as a whole
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
  whose command or other attributes change needs its hash taken again. A set is picked the way
  [`[command]`](#os-specific-command-selection) picks an argv: `windows`,
  `macos` or `linux` when present, `default` otherwise. A WSL profile, whose
  CLI runs on Linux, carries `default` alone. No placeholder is substituted in
  it. Each entry must be a non-empty table, and at least one set must be
  present. Missing tables on the path are created; a key on the path that
  holds something other than a table fails the step and nothing is written.
  `retired` lists keys an earlier version wrote under `under` and this one
  no longer does -- the bundled Codex profiles list the trust of two
  `UserPromptSubmit` handlers they no longer have -- each list picked like
  the sets. Register, Update, Remove and a rewrite take those tables out; the
  state read does not look at them, so one left behind does not make the row
  differ. A key must not be empty.
- `json_entry` writes entries of a JSON object, one element per file: `file`
  is home-relative and `entries` names each entry. `file` may carry
  `{server}`, its only placeholder, for a CLI whose hooks file has no place
  to keep one instance's entries apart from another's: each instance then
  has a file of its own (the bundled Grok Build profiles write
  `.grok/hooks/{server}.json`), every entry in it belongs to that instance
  whatever its key says, and such a file cannot hold the `claim`. The path
  must stay home-relative, with no `..`, once `{server}` is filled in. A key that starts with
  `/` is a JSON Pointer (RFC 6901) into nested objects, whose missing parents
  are created as objects; any other key names a top-level entry. A key may
  carry `{server}`. An optional `by_os` table adds the entries of one set,
  picked like `toml_set.entries`' sets (`windows`, `macos` or `linux` when
  present, `default` otherwise); a set may not repeat a key of `entries`.
  A value is any TOML value, written as JSON, whose strings may carry
  `{daemon}`, `{bridge}` and `{server}`; a string that is exactly
  `{env_refs}` becomes the bridge's environment: the window's identity as a
  reference that is empty where it is unset (`{"YAGURA_WINDOW_ID":"${YAGURA_WINDOW_ID:-}",
  "YAGURA_STATUS_TOKEN":"${YAGURA_STATUS_TOKEN:-}"}`), never its value, and for a named instance
  `YAGURA_INSTANCE` by value. Every session of the CLI reads the entry, inside a window or not,
  and Muse turns off all of a session's MCP servers when one names an unset variable. `{daemon}` is
  replaced by the absolute path of the daemon's copy in its report folder,
  and `{bridge}` by the bridge's path -- both converted by the profile's
  `[host_paths]` when it has one -- and a `{daemon}` path containing
  whitespace, `'`, `` ` ``, `$`, or a character the command interpreter would
  act on is not written at all, since a hook command carries it unquoted. The
  rest of the file is kept byte for byte, a missing file is created holding
  only these entries, and a file that is not a JSON object, or a pointer
  whose parent holds something other than an object, is left untouched with
  the step failed.

Both edits read and write through the row's file tools -- `files.read` /
`files.replace`, or `[host_paths]`'s `read` / `replace` -- when it has them,
and are written atomically otherwise, the same way `toml_set` is.

**Registering without a command.** A CLI with no `mcp add` of its own
(Muse Code) leaves out both `add` and `remove` -- one without the other is an
error -- and then `env_arg` and `env_forward` too. The registration is then
made by the `json_entry` alone, which must hold the bridge's entry: a key
ending in `{server}` whose value holds a string that is exactly `{bridge}`.
For a named instance that entry must carry `{env_refs}`, or Register is
refused as unable to carry the instance. The GUI shows such a row with the
files it writes and their entries in place of a command.

**Shared entries and the claim.** An entry whose key has no `{server}`, in a
file whose name has none either, is the same for every instance, so a CLI that has room for only one such value --
Muse Code's `managed_hooks_path` -- can serve all of them. A registration with
shared entries must name one of them as its `claim`: a shared entry holding a
plain value, at most one per registration.

- Register and Update read the claimed entry first. When the file holds
  another value there -- one of the user's own -- nothing is written, the
  step fails naming the file and the entry, and the row's state says so
  ahead of time, so the GUI turns Register and Update off (see
  [Registering from the GUI](../docs/mcp-setup.md#registering-from-the-gui)).
  When it holds no value or this registration's own, the shared entries are
  written over.
- Remove takes out this instance's entries, and the shared ones only when the
  claim holds this registration's value and no other instance's bridge entry
  is left -- judged the same way as under "Turning the other instances'
  registrations off" below. A file that cannot be read keeps them. The files, and
  any object the removal empties, stay.

**Which file is written first.** When a registration writes more than one
file, the file holding the claim refers to the others (Muse Code's
`settings.json` names the hooks file), so it is written last and emptied
first; the other files keep the profile's order. A step that fails stops the
operation there and nothing is undone, as for every row: whichever step
fails, no file refers to one that was never written, the row reads
**differs**, and Update puts it right.

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
The [`[host_paths]`](#host-paths) conversion, `files.read` and
`files.replace` are different: they run as their own standalone commands,
because they have nothing to do with launching the CLI.

Every one of these commands starts in an empty folder the daemon keeps for
them (`empty`, under the instance's state folder, created again whenever it
is missing), never in the home directory: a CLI reads the settings of the
folder it starts in as a project's, and the home directory read that way
would mix a project's registration into the row -- from WSL, the Windows
CLI's own -- and point `remove` at it. When the folder cannot be made, or
the daemon found no home directory, the command is refused. A WSL command
whose folder `wsl` cannot translate starts in the distribution's home.

**Reading a row's state.** `status` names either a command to run (`args`,
read the same way a window's launch is) or a settings file to read (`file`,
home-relative; inside another environment, through `files.read` instead of
directly). The result is one of five states: **registered** (the CLI's own
record names exactly this bridge, with every `require` / `toml_set.confirm`
value and every owned entry in place), **differs** (the record exists but
names another bridge, is missing a required value, or lacks an owned entry or
holds another value in it -- `differs_in` names which, giving owned entries
as `under` joined by `.` or as the `json_entry` keys with `{server}` filled
in; a claimed entry held by another value is reported here too, with the
claim's pointer as `refused_by_claim`), **not registered**
(`absent` matched the command's output, the settings file does not exist, or
a `files.read` tool exited with `absent_exit`), **not installed** (the CLI
itself, or its `[host_paths]` conversion, could not be found or run), or
**unknown**
(the output could not be read as the declared format -- never guessed as
registered or not).

A CLI with no command that shows one server (Grok Build has `grok mcp list
--json`, an array, and no `get`) is read with `items` and `name` in a
`"json"` `status`, the same two pointers `list` takes below, given together:
the element whose name is the registration's is the record that `command`,
`args_pointer` and `toml_set.confirm` then point into, and no such element
means **not registered**. Output with no array at `items` is **unknown**.

**A notice on the row.** `notice` is one line of visible text the GUI shows
on the row, before and after registering, for an effect the user should know
of before pressing Register -- the bundled Grok Build profiles use it to say
that a registered hook makes Grok Build ask about folder trust in every
session. It follows the rule a `[shim_guidance]` `text` follows; a value
outside it fails the table. The daemon reads nothing in it.

**Turning the other instances' registrations off.** A CLI that hands an MCP
server only the variables its own configuration names starts a registered
`yagura` without the window's identity, so a window of a named instance would
reach the default instance's daemon as an outside client. A CLI that hands
it the window's whole environment still starts every `yagura` registered
with it, so the window would carry another instance's bridge beside its own;
the bundled Copilot tables turn those off too. Grok Build has no launch
argument that leaves a registered server out, so its tables refuse those
servers' tools instead (`--deny MCPTool({server}__*)`): the servers still
start, and their tools are still listed by name, but cannot be called
(Grok Build 1.0.70). When a profile
declares `list`, the daemon runs `args` after the profile's launch argv (with
no model or effort added), in the window's own working directory so it sees
what the window's CLI sees, each time it opens a window from the profile, and
appends `disable` once per registered server that is yagura's bridge for some
other instance: its name is `yagura` or `yagura-<instance>` with a valid
instance identifier, **and** the file name of the command it runs is the
file name of this daemon's bridge. The server the window's own `[mcp]`
injection is named for stays on, and a server of your own is never touched.
`{server}` is the only placeholder, `args` takes none, and `disable` must
carry it. The wait for a free slot and the listing together are limited to 10
seconds; when the listing fails, times out or is not the declared shape, the
daemon logs a warning and opens the window without turning anything off. A
profile whose `[mcp]` table spells the server name out instead of `{server}`
is left as it is, since the daemon cannot tell which name it injects.

**Out-of-date entries.** When the owned entries no longer match what the
profile would write -- after an update changed them, say -- the daemon writes
them again itself, each time it opens a window from the profile and each time
it lists the rows for Settings > Integrations, but only while the instance is
still registered: a file must hold something this instance's Remove would
take out. That is derived from the table, not from the CLI's name: a
`json_entry` key carrying `{server}` (or any owned key, in a file whose name
carries `{server}`), or the `toml_set` `table` when it carries `{server}` and
the table owns `entries`. Only the owned entries are written, through the same
step Register uses, with `retired` taken out; `add`, `remove` and
`toml_set.values` are left alone, and nothing is written while the claim holds
another value. The listing does this only for a row it reads as registered or
differing. A rewrite waits for a user's Register, Update or Remove for at most
the listing's time limit and otherwise gives up; a window whose entries could
not be written opens without its hooks, and the daemon logs why. See
[Registering from the GUI](../docs/mcp-setup.md#registering-from-the-gui).

**Update and Remove can drop a hand-written value.** A CLI's own `add`
command can rewrite its whole record rather than editing it in place: the
bundled `codex-cli` and `wsl-codex-cli` tables' `add` drops any key its own
`-c mcp_servers.{server}.*` overrides did not set, including a
`tool_timeout_sec` an earlier `toml_set` edit wrote -- which is why their
`[mcp_registration.toml_set]` writes it back once, right after `add`. Update
checks the claim, then runs the steps of Remove and then those of Register.
Remove runs `remove` (when the CLI has commands), then takes out the owned
entries (`toml_set.entries`, `json_entry`); it leaves `toml_set.values`
alone.

## WSL profiles

The six WSL profiles this repository ships (`wsl-claude-code`,
`wsl-codex-cli`, `wsl-antigravity-cli`, `wsl-copilot-cli`, `wsl-muse-code`,
`wsl-grok-build`) start the CLI with

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

Each of them also converts the paths the daemon hands its CLI with WSL's own
tool ([Host paths](#host-paths)):

```toml
[host_paths]
convert = ["wsl", "-e", "wslpath", "-u", "{path}"]
```

Their `[mcp]` and `[hooks]` tables, where they have them, are written inline
and name no path of a Linux file (the credentials file of a `"launch"` profile
is named by its Windows path, since the Windows `yagura-daemon.exe` and
`yagura-mcp.exe` read it), so opening one of their windows runs no conversion -- except
`wsl-copilot-cli`'s `[hooks]`, a plugin directory Copilot takes only by its
path, and `wsl-muse-code`'s `[mcp]` (`{bridge_json}`) and `[hooks]` (a
managed hooks file Muse takes only by its path), which are converted at each
launch. Their registration rows convert too. Those that read the CLI's own
files -- a registration's, or `wsl-muse-code`'s `absent` guard -- read them
with `[host_paths]`'s `read`.

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
profile since -- and its MCP injection, hooks and `[delivery]` table -- apply to the
restored window.

Only when the daemon refuses that launch (for example, the profile no longer
exists, or no longer runs on this operating system) does the GUI fall back
to re-spawning the saved argv, detection definition and `vendor` as they
were recorded; a restored window opened that way gets no MCP injection and takes no part in
[delivery](#delivery) --
except when the refusal is `profile_disabled` (see
[Disabling a profile](#disabling-a-profile)): retrying the saved argv would
launch the very CLI the user switched off, so that window is left unopened
instead, and its name is collected into a single notice covering every
window the current restore held back this way. Any other failure is not
retried. A window opened from a raw argv, or recorded by a daemon from
before the snapshot kept the profile, is always restored from its saved
argv.
