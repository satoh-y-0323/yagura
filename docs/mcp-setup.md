# MCP Setup Guide

This guide explains how to set up the yagura MCP server and connect it to Claude Code or other MCP clients.

## Overview

yagura exposes a parent MCP server that allows external agents to manage multiple PTY windows running arbitrary commands. The MCP server communicates with the yagura daemon over a local IPC endpoint: a named pipe on Windows, and a Unix domain socket on macOS and Linux.

## Prerequisites

- Windows, macOS or Linux
- yagura daemon (`yagura-daemon`, `yagura-daemon.exe` on Windows) already running on the same
  machine and user account
- MCP-compatible client (e.g., Claude Code, Codex)

The Windows installer ships `yagura-daemon.exe` and `yagura-mcp.exe` next to the GUI executable and
installs for the current user only, so it needs no administrator rights. It carries no code-signing certificate yet:
SmartScreen warns about an unrecognized publisher, and you have to choose "More info" -> "Run anyway"
to proceed. It also needs the Microsoft Edge **WebView2** runtime, which it downloads and installs
silently when it is missing -- so an installation on a machine without WebView2 requires network
access (Windows 11 ships WebView2 preinstalled).

The Windows installer is the only packaged build. On macOS and Linux, build yagura from source; the
daemon and the MCP server work the same way there, over a Unix domain socket instead of a named pipe.

## Starting the Daemon

Before using the MCP server, start the yagura daemon:

```bash
yagura-daemon
```

The daemon initializes with one default shell window and listens for IPC connections.

The daemon outlives the GUI: closing the GUI leaves it running, still serving the version it was
started with. **After upgrading yagura, restart `yagura-daemon`** (`yagura-daemon --shutdown`, then
start the GUI again). A client checks the wire revision the daemon reports before sending a request
that needs a newer one: when a daemon from before the upgrade answers with an older revision, such
a call is refused outright as `unsupported` rather than sent. Restarting the daemon closes every
window it is currently running.

## Connecting to a Second Instance

`yagura-mcp` reads the `YAGURA_INSTANCE` environment variable, exactly as `yagura-daemon` does, and
connects to that instance's endpoint rather than the default one. To register a second MCP server
pointed at a development instance, set `YAGURA_INSTANCE` on the process that launches `yagura-mcp`
(for example in the MCP client's own server configuration, alongside `command`), matching the value
the corresponding daemon was started with. An invalid value is refused the same way an invalid
value is refused when starting the daemon: `yagura-mcp` writes the reason to stderr and exits
without connecting to any daemon, rather than falling back to the default instance. See
[docs/instances.md](instances.md) for the full specification, including the rules an identifier
must follow.

[Registering from the GUI](#registering-from-the-gui) does this for you. The GUI of a named instance
registers the bridge as `yagura-<instance>` rather than `yagura`, and carries
`YAGURA_INSTANCE=<instance>` in the registration's own environment (`-e` for Claude Code, `--env`
for Codex CLI and Antigravity CLI), so a development instance's registration never overwrites, and
is never overwritten by, the default instance's `yagura`. The default instance registers as
`yagura` with no environment. Register by hand the same way: pick a name of your own and pass
`YAGURA_INSTANCE` through the CLI's environment flag.

## Windows yagura Opens

**A Claude Code or Codex CLI window that yagura opens from one of its bundled launch profiles needs
no MCP registration.** The daemon points that CLI at the bridge itself:

- At start-up, the daemon looks for `yagura-mcp` next to its own executable. The installer ships
  `yagura-mcp.exe` there, beside `yagura-daemon.exe`. When none is usable, the daemon logs
  `no usable mcp bridge next to the daemon; windows open without MCP injection` and every window
  opens without it.
- Every window's child gets `YAGURA_MCP_BRIDGE`, the bridge's absolute path, beside
  `YAGURA_WINDOW_ID` and `YAGURA_STATUS_TOKEN`.
- A window launched from a profile with an `[mcp]` table gets that table's arguments appended to
  its command line: Claude Code a `--mcp-config` server definition, Codex CLI `-c` overrides for
  `mcp_servers.yagura` that also forward the channel variables with `env_vars`. The bundled
  `claude-code`, `codex-cli`, `wsl-claude-code` and `wsl-codex-cli` profiles carry one; `shell`
  does not, and neither does `antigravity-cli` or `wsl-antigravity-cli` -- Antigravity CLI
  (`agy`) reads MCP servers only from its own global registration, not from anything passed at
  launch, so a window opened from either of those two profiles needs the manual registration
  described in [Configuring Antigravity CLI](#configuring-antigravity-cli) below, the same as a
  session yagura did not start. What the table may contain, and when the daemon drops the
  injection instead, is in [profiles/README.md](../profiles/README.md#mcp-injection).

This covers every launch from a profile: `open_window` with `profile`, a `send_handoff` that opens a
new window from a `profile`, `open_task_window`, a restored window reopened from its profile, and
the GUI's add-window form with a profile selected. A window started from a raw command line is not
covered.

The bundled `[mcp]` table applies even when you have changed the profile: your file in the config
root carries only what you changed and is laid over the bundled profile (see
[profiles/README.md](../profiles/README.md#where-profiles-live)).

If the CLI also has a server named `yagura` registered, the injected one is what runs in a window
yagura opens: Claude Code starts the `--mcp-config` definition instead of a same-named one from a
project's `.mcp.json`, and Codex CLI merges the `-c` overrides into an existing
`[mcp_servers.yagura]` table key by key, keeping the keys the injection does not set. The bundled
Codex profiles set `tool_timeout_sec` themselves (see [Configuring Codex CLI](#configuring-codex-cli)
for why), so a value in `~/.codex/config.toml` does not apply to a window yagura opens.

**Registration is still needed** for a session yagura does not start -- for example the Claude Code
you run in your own terminal to drive yagura's windows -- and for a CLI that cannot be handed an MCP
server at launch, that is, one opened from a profile without an `[mcp]` table.
[Registering from the GUI](#registering-from-the-gui) covers those cases in one place; the
per-CLI sections after it are the manual alternative.

**Status reports come with the same profiles.** The bundled Claude Code profiles also give the CLI
hooks that report each turn's start and end to the daemon, so the window's status no longer depends
on the screen alone. The Codex CLI profiles do the same only once yagura is registered with Codex
CLI, and Antigravity CLI reports only once yagura is registered with it -- both from
[Registering from the GUI](#registering-from-the-gui). What the hooks report, and the mark a window
shows when its reports stop arriving, are in
[Detection definitions](../crates/yagura-daemon/detection/README.md#structured-sources---emit-status).

**Whether it worked** shows in [`list_windows`](#list_windows) and
[`get_window_status`](#get_window_status) as `mcp_connected: true`, and in the GUI as a plug icon
beside the window's name, in the status panel and the title bar, with the tooltip
"Connected to yagura". The bridge identifies itself to the daemon as soon as the CLI starts it,
before its first tool call.

**Building from source on a development machine**: build the GUI first, then `yagura-daemon`, then
`yagura-mcp`. A GUI build copies the staged sidecar binaries over the `yagura-daemon` and
`yagura-mcp` executables in the same cargo target directory, and the daemon uses whichever
`yagura-mcp` sits next to it, so building the two last keeps both current. Restart the daemon
afterwards: it resolves the bridge only when it starts.

## Registering from the GUI

The GUI's Settings panel (the gear button in the navigation rail) has an **Integrations** tab that
registers the bridge beside the running daemon with each CLI, so a session you start yourself can
reach yagura. It lists one row for every launch profile with a `[mcp_registration]` table (see
[profiles/README.md](../profiles/README.md#mcp-registration)): the bundled `claude-code`,
`codex-cli` and `antigravity-cli` under "This computer", and `wsl-claude-code`, `wsl-codex-cli` and
`wsl-antigravity-cli` under "WSL". `shell` has no row.

Each row shows the command it is about to run and the CLI's current state:

- **Registered**: the CLI's own record names this bridge (and, for Codex CLI, carries
  `tool_timeout_sec = 86400`), and the hook settings described below are in place. **Remove** is
  offered.
- **Not registered**: **Register** runs the CLI's own `mcp add`.
- **Registered to a different bridge**, or **Registered with a different** followed by what
  differs (`tool_timeout_sec`, `hooks.state` for Codex CLI's hook trust, or `yagura` for
  Antigravity CLI's hooks entry): **Update** runs `mcp remove`, then the same steps as Register.
  **Remove** is offered too.
- **Not installed**: the CLI (or, for a WSL row, WSL itself) could not be found or run. Register
  cannot be pressed.
- **Unknown**: the CLI's answer could not be read. It is never guessed as registered or not;
  Register and Remove are both offered.

**What yagura runs and writes.** yagura runs each CLI's own `mcp add` and `mcp remove` -- through
the profile's own launch command, so a WSL row reaches the CLI inside the distribution, with the
bridge's path converted by `wslpath` -- in your home directory, so no project's `.mcp.json` is
picked up. Claude Code is registered with `--scope user`. yagura writes three things itself,
changing nothing else in the file and keeping a symlinked file a symlink and its permissions as
they were:

- **Codex CLI's `tool_timeout_sec`**: right after `codex mcp add`, it sets `tool_timeout_sec =
  86400` in the `[mcp_servers.<name>]` table that `codex mcp add` just wrote to
  `~/.codex/config.toml` (see [Configuring Codex CLI](#configuring-codex-cli) for why). It never
  creates that file or that table: when the table is not there after `add` -- for example because
  `CODEX_HOME` points Codex CLI at another file -- that step fails, `add` is left in place, and the
  row reads **Registered with a different `tool_timeout_sec`** until Update succeeds.
- **Codex CLI's hook trust**: in the same write, one `[hooks.state."<key>"]` table per hook the
  bundled Codex profile gives a window, holding the `trusted_hash` Codex checks before it runs that
  hook -- the same place Codex records your own "Trust all". Codex runs yagura's hooks in a window
  only while these are in place.
- **Antigravity CLI's hooks entry**: an entry named after the registration (`yagura`, or
  `yagura-<instance>`) in `~/.gemini/config/hooks.json`, which runs the daemon on each model call
  and after each tool. The file is created when it is missing; one that is not valid JSON is left
  untouched and the step fails.

**Remove** takes the hook trust tables and the hooks entry out again, along with the CLI's own `mcp
remove`; it leaves `tool_timeout_sec` alone. Any failed step stops the operation there. The output
of `add` and `remove` is shown in the row; what a state read returns is neither shown nor logged.

**Update and Remove discard what you wrote by hand in Codex CLI's table.** `codex mcp remove`
deletes the whole `[mcp_servers.<name>]` table, and `codex mcp add` rewrites it, so any key you
added to it yourself -- such as `env_vars` -- is gone afterwards; yagura writes back only
`tool_timeout_sec`. Add such keys again after an Update.

**Reading the state.** The rows are read when the tab opens and after each operation, never in the
background: `claude mcp get <name>` for Claude Code, `codex mcp get <name> --json` for Codex CLI,
and `~/.gemini/config/mcp_config.json` (read, never written) for Antigravity CLI, plus the hook
settings above, read back from `~/.codex/config.toml` and `~/.gemini/config/hooks.json`. `claude mcp get`
starts the bridge briefly to check that it connects; that connection is `External` and changes no
window's `mcp_connected`.

**A named instance** registers as `yagura-<instance>` and carries `YAGURA_INSTANCE` (see
[Connecting to a Second Instance](#connecting-to-a-second-instance)).

**A CLI session that is already running** does not see a change until it is restarted.

Only the GUI can do this: the daemon accepts these requests from a registered approver alone (see
[Approver Registration](#approver-registration)), and there is no MCP tool for them. When the
daemon has no bridge beside it, the tab says so and reads no row.

## Configuring Claude Code

This section and the two after it are the manual alternative to
[Registering from the GUI](#registering-from-the-gui): for a machine without the GUI, a
registration scope other than the user's, or a CLI whose profile has no `[mcp_registration]` table.
Either way, the registration is for a session yagura does not start, or a CLI yagura cannot hand an
MCP server at launch (see [Windows yagura Opens](#windows-yagura-opens)).

### 1. Locate yagura-mcp

The installer puts `yagura-mcp` beside `yagura-daemon`. With the Windows installer's default
per-user location, that is `%LOCALAPPDATA%\yagura\yagura-mcp.exe`. When building from source (the
only way to get it on macOS and Linux), compile it with:

```bash
cargo build -p yagura-mcp --release
```

The executable will be at `target/release/yagura-mcp` (`yagura-mcp.exe` on Windows).

### 2. Register it

Register the bridge's absolute path with Claude Code's own command. `--scope user` makes it
available in every directory you start Claude Code from:

```bash
claude mcp add --scope user yagura -- <absolute path to yagura-mcp>
```

Use `--scope local` (the default) to limit it to the current project, or `--scope project` to
write it to the project's `.mcp.json` for everyone who works in that repository. Claude Code does
not read MCP servers from `settings.json` or `settings.local.json`.

yagura writes to a CLI's own configuration only when you press a button in
[Registering from the GUI](#registering-from-the-gui), and then only through the CLI's own
`mcp add` / `mcp remove` -- plus `tool_timeout_sec` in Codex CLI's `[mcp_servers.<name>]` table.
The commands in this section are ones you run yourself.

### 3. Confirm it

Run `claude mcp list`, or `/mcp` inside Claude Code, to confirm `yagura` is listed and connected.
`claude mcp remove --scope user yagura` undoes the registration.

## Configuring Codex CLI

A handoff can only be replied to from inside a window if that window's CLI can reach yagura-mcp as an MCP server (see [`send_handoff`](#send_handoff)). A Codex CLI window yagura opens from a profile with an `[mcp]` table already can (see [Windows yagura Opens](#windows-yagura-opens)); register yagura for any other Codex CLI session -- from [Registering from the GUI](#registering-from-the-gui), which does steps 1 and 2 for you, or by hand as below. A window yagura opens gets step 2's `tool_timeout_sec` from its profile, so step 2 is only for the sessions you register yagura for yourself.

1. Locate `yagura-mcp` as in [Configuring Claude Code](#1-locate-yagura-mcp), then register it (Codex CLI 0.152.1):

   ```bash
   codex mcp add yagura -- <absolute path to yagura-mcp>
   ```

2. Codex CLI's own per-call tool timeout (`tool_timeout_sec`, default 60 seconds) is shorter than the `timeout_sec` that [`wait_for_status`](#wait_for_status) and [`wait_for_report`](#wait_for_report) accept, and than a `send_message` or `send_handoff` can legitimately take. The daemon bounds those two waits itself, but not a delivery: a `send_message` or `send_handoff` waits for as long as the receiving CLI takes to show the text, and ends otherwise only on one of the events listed under [Maximum wait](#send_handoff). A timeout that gives up on the call does not stop the delivery in the daemon unless the client also cancels the request. So lift Codex's own limit out of the way rather than tuning it to these waits, and end a stuck delivery the ways that section lists. Edit `~/.codex/config.toml`:

   ```toml
   [mcp_servers.yagura]
   command = "<absolute path to yagura-mcp>"
   tool_timeout_sec = 86400  # a day: well beyond any wait the daemon bounds,
                             # the same value the bundled Codex profiles pass
   ```

3. **A registration needs no `env_vars`.** A session you start yourself runs outside any window, so it has no window ID or token to forward, and a named instance's `YAGURA_INSTANCE` travels in the registration's own environment instead (`codex mcp add --env YAGURA_INSTANCE=<instance> ...`, which writes `[mcp_servers.<name>.env]`; see [Connecting to a Second Instance](#connecting-to-a-second-instance)). A window yagura opens from a bundled Codex profile receives `env_vars` at launch through its `[mcp]` injection (see [Windows yagura Opens](#windows-yagura-opens)). `env_vars` matters only for Codex CLI started *inside* a yagura window without that injection -- from a raw command line, or a profile with no `[mcp]` table. Codex CLI does not hand its own environment to a stdio MCP server: it builds the server's environment from a fixed allow-list (`WINDOWS_CORE_ENV_VARS` on Windows, `HOME` / `PATH` and friends on Unix) plus whatever `[mcp_servers.<id>].env` and `env_vars` name, so the `YAGURA_WINDOW_ID` / `YAGURA_STATUS_TOKEN` that the daemon injected into the window's process tree never reach `yagura-mcp` on their own. Every call made from such a window is then `External` (see [Requester Identification](#requester-identification)): it always raises a human approval dialog, its `send_handoff` envelope carries `"from":"external"` with no `reply` key to answer on, and even a `send_message` aimed at that same window is not recognized as self-addressed. To have it identify as its window, add `env_vars = ["YAGURA_WINDOW_ID", "YAGURA_STATUS_TOKEN", "YAGURA_INSTANCE"]` to the table by hand -- and again after any Update from the GUI, which discards it (see [Registering from the GUI](#registering-from-the-gui)).
4. Run `codex mcp list` to confirm the server is registered and reachable.
5. `codex` itself must be resolvable on `PATH` for a `send_handoff` targeting a `codex-cli` launch profile to spawn successfully -- a `profile` whose command cannot be found fails later, at spawn, as `spawn_failed`.
6. **Codex's own trust confirmation is not detected by yagura.** The first time Codex CLI runs in a working directory it has not seen before, it shows a directory/hooks trust dialog on its own terminal; yagura's shipped detection definitions do not recognize this prompt, so the window's status stays `unknown` forever and a handoff can never reach it (`window_not_ready`, or `status_unknown` for an already-open window). Launch Codex by hand once in whatever `cwd` a handoff will target, and clear that dialog, before relying on a handoff to reach it.

## Configuring Antigravity CLI

Antigravity CLI (`agy`) reads MCP servers only from its own global registration
(`~/.gemini/config/mcp_config.json`), never from anything passed at launch, so a window
opened from the bundled `antigravity-cli` or `wsl-antigravity-cli` profile needs this
registration too -- there is no `[mcp]` table for either of them to carry one (see
[Windows yagura Opens](#windows-yagura-opens)). [Registering from the GUI](#registering-from-the-gui)
does it for you, with the bridge's resolved path; the commands below are the manual alternative.

**Windows** (`agy` 1.2.11): register the bridge's own absolute path directly, not a `cmd
/c` wrapper around the `%YAGURA_MCP_BRIDGE%` environment variable that a window yagura
opens carries. `agy` builds its server's Windows command line from its `command` /
`args` registration without quoting an element that itself contains a space, so
`cmd /c %YAGURA_MCP_BRIDGE%` fails to start whenever the bridge's own install path
contains one (which the default per-user install location can) -- register the resolved
path instead:

```bash
agy mcp add yagura -- <absolute path to yagura-mcp.exe>
```

**WSL**: `agy`'s Linux build can read the bridge path through the `YAGURA_MCP_BRIDGE`
environment variable a WSL window yagura opens carries (translated to a Linux path by
`WSLENV`, the same way [`wsl-codex-cli`](../profiles/README.md#wsl-profiles) forwards
it), through a shell that reads it at startup rather than a literal path baked into the
registration:

```bash
agy mcp add yagura -- bash -c 'exec "$YAGURA_MCP_BRIDGE"'
```

1. Run `agy mcp list` (or open agy's own `/mcp` screen) to confirm `yagura` is registered
   and shows `Tools:` rather than an error -- the same command reports whether the
   registration itself is broken, before any window is involved. Whether a particular
   *window* connected is a separate question, answered the same way as for any other CLI
   (see [Windows yagura Opens](#windows-yagura-opens)): `mcp_connected: true` and the
   GUI's plug icon. `agy mcp remove yagura` undoes the registration.
2. **A registration that names `%YAGURA_MCP_BRIDGE%` or `$YAGURA_MCP_BRIDGE` only starts
   successfully inside a window yagura opened** -- that variable is one of the ones the
   daemon injects into a window's own process tree (see
   [Windows yagura Opens](#windows-yagura-opens)), so `agy` started by hand at a plain
   terminal, or from any other launcher, has no value for it and the server fails to
   start, showing as an error in `agy`'s own `/mcp` screen. Register the bridge's
   resolved absolute path instead (as in the Windows form above) for a session you want
   to reach yagura from outside a window it opened.
3. **`agy` asks for approval before every call to a yagura tool**, not just the first
   one in a session: its own approval dialog offers to always allow that tool for the
   rest of the conversation, or to persist that choice to `settings.json` so it carries
   across sessions -- whether to keep either is your call, not something yagura sets for
   you. Until a call is allowed, it sits waiting on that dialog: a `send_handoff` reply
   from an `agy` window is not delivered until the tool call it needs is approved on that
   window's own screen.

## Using the MCP Tools

Once connected, you can use the following tools:

### list_windows

Returns a list of all active and exited windows managed by the daemon.

**Input:** None

**Output:**
```json
{
  "windows": [
    {
      "window_id": "w1-k3f9xa",
      "name": "pwsh",
      "vendor": null,
      "cwd": "C:\\Users\\you",
      "status": "running",
      "exit_code": null,
      "room": { "room_id": "r-…", "name": "yagura" },
      "mcp_connected": false
    },
    {
      "window_id": "w2-k3f9xa",
      "name": "codex",
      "vendor": "codex",
      "cwd": "C:\\Users\\you",
      "status": "unknown",
      "exit_code": null,
      "room": { "room_id": "r-…", "name": "yagura" },
      "unknown_reason": "no_rule_matched",
      "mcp_connected": true
    }
  ]
}
```

`room` is omitted entirely for a window whose `status` is `exited`.

`unknown_reason` is present only on an entry whose `status` is `unknown`, and names one of six reasons (`no_definition`, `no_rule_matched`, `screen_lost`, `detector_stopped`, `not_yet_judged`, `declared_unknown`); what each one means and what it asks of you is in [Why a window is `unknown`](#why-a-window-is-unknown). `detection_warning` is present on a window that is running no detection definition -- no layer carries its name, the layer that would carry it could not be read, or the daemon has no definition layer configured at all -- and carries the same text [open_window](#open_window) returned when the window was opened. Either key being absent -- they are omitted, never sent as `null` -- means there is nothing to report.

`mcp_connected` says whether a yagura MCP bridge has identified itself as that window over a connection that is still open:

- `true`: the window's CLI is running a bridge the daemon has verified with that window's own token, so the window can call yagura's tools as itself -- reply to a handoff, post to the chat, `send_report`.
- `false`: no such bridge right now. The CLI has not started one (yet), it was started without the window's variables, its `identify` was refused, or it has exited. A window opened from a profile without an `[mcp]` table, or whose injection was dropped (see [Windows yagura Opens](#windows-yagura-opens)), stays `false` unless its CLI has yagura registered.

### get_window_status

Get detailed status and screen content summary for a specific window.

**Input:**
```json
{
  "window_id": "w1-k3f9xa"
}
```

**Output:**
```json
{
  "window_id": "w1-k3f9xa",
  "status": "running",
  "since_ms": 1234,
  "last_output_summary": "C:\\> ",
  "exit_code": null,
  "mcp_connected": false
}
```

The `last_output_summary` contains up to 500 characters from the end of the text [`read_output`](#read_output) returns. `mcp_connected` means the same as in [list_windows](#list_windows).

A window whose `status` is `unknown` carries `unknown_reason` as well, and one running no detection definition carries `detection_warning` -- the same two keys [list_windows](#list_windows) reports, with the same values and the same omission rule:

```json
{
  "window_id": "w1-k3f9xa",
  "status": "unknown",
  "since_ms": 4210,
  "last_output_summary": "",
  "exit_code": null,
  "unknown_reason": "no_definition",
  "detection_warning": "failed to load the detection definition (no detection definition named \"claude-code\" was found in the directory C:\\Users\\you\\detection); status stays unknown",
  "mcp_connected": false
}
```

### send_message

Inject text into a window's input stream.

**Input:**
```json
{
  "window_id": "w1-k3f9xa",
  "text": "ls -la\n",
  "mode": "immediate"
}
```

**Output:**
```json
{
  "ok": true
}
```

`text` is a single line. A trailing newline submits it; text without one is typed into the window but not submitted. The line ending -- `\n` or `\r\n` -- is normalized to a lone `\r` before it reaches the window, because a console line editor (Windows PSReadLine in particular) submits on CR: a bare LF only opens a line continuation, and a CRLF submits on the CR and then inserts a newline from the LF, leaving a stray `>>` continuation prompt behind. A lone `\r` is left as it is, and no terminator is ever appended.

A body with a line break anywhere but that single trailing position -- a `\r` or `\n` in the middle of the text, or a second terminator at the end -- is rejected as `invalid_argument` (`send_message text contains a line break at byte offset <n> that is not the body's single trailing terminator`), and so is a body containing any other control character (for example `ESC`). Both are checked before any approval decision is made. The daemon writes a body straight through with no per-line confirmation, so it could not tell whether a multi-line body reached the receiving CLI as one message or as several; an unfiltered control character could redraw the receiving window's screen and defeat the daemon's own read of it. To hand a window a multi-line request, put it in a plan task's `instruction` (see [create_plan](#create_plan)) and open the task's window with [open_task_window](#open_task_window): the window is told to read the request back with [get_plan](#get_plan), and the request itself is never typed into its terminal (see [Task instructions](plans.md#task-instructions)). Several separate lines can also be sent as separate `send_message` calls.

Text ending in exactly one terminator -- a single trailing `\r`, `\n` or `\r\n` -- is written according to the receiving CLI's input definition (its `[input]` table, see [Detection definitions](../crates/yagura-daemon/detection/README.md#schema)): the body and the submit key are written as separate steps, with the definition's settle interval between them, and the trailing terminator is replaced by the definition's own submit key. The definition's `probe` is typed behind the body, and the submit key is written only once the composer shows the body handled (see [Detection definitions](../crates/yagura-daemon/detection/README.md#input)); a window whose definition names no `probe` refuses such text as `no_composer`. Until the composer shows it, the call waits with no time limit of its own, and ends otherwise only on one of the events listed under [Maximum wait](#send_handoff) -- cancelling the call among them, which takes the written body back out of the composer when the definition can. Text with no terminator, or only a terminator, is written through unchanged, and needs no `probe`. It is written only once the receiving CLI has finished painting its screen -- the cursor is shown and no synchronized update (`CSI ? 2026 h`) is open -- so it never lands on a dialog caught half-drawn; until then the call waits with no time limit of its own, and ends otherwise only when the requester leaves or the window's child exits (see [Maximum wait](#send_handoff)). Such text is not written (`not_written`) on a screen the definition's rules read as an approval prompt or declare `unknown`, whether or not that screen is finished: a dialog keeps the cursor hidden. Unlike `send_handoff`, `send_message` never waits for a delivery reaction and never re-sends: the reply comes back once the submit key is written, and the text is written exactly once. It takes no turn in the queue `send_handoff` deliveries wait in, but its write never lands in the middle of a delivery's attempt: it waits for that attempt to finish first, the same as it waits for another `send_message` in progress (see [Maximum wait](#send_handoff)).

The daemon does not hold up the requester's connection while a `send_message` waits: other requests on the same IPC connection are still served, and one connection may have as many `send_message`, [`wait_for_status`](#wait_for_status) and [`wait_for_report`](#wait_for_report) calls outstanding together as its limit on pending waits allows; past it, `send_message` is refused as `resource_exhausted`. A requester that disconnects ends its waiting `send_message` the same way a cancel does.

`send_message` is a gated call (see [Approval Flow](#approval-flow)): it succeeds immediately when the caller is the target window itself or a registered approver, and is otherwise held pending a human decision unless the target's status is `unknown`, in which case it is refused outright as `status_unknown` -- even for a caller sending to its own window, since the daemon cannot tell whether firing input is safe. A window whose detection definition never matches its own output stays `unknown` forever, so it can never receive `send_message` at all (see [Troubleshooting `status_unknown` and `child_exited`](#troubleshooting-status_unknown-and-child_exited) for workarounds). A window whose definition declares no composer -- a plain shell, or a command opened without an agent CLI's definition (see [Detection definitions](../crates/yagura-daemon/detection/README.md#composer)) -- is refused as `no_composer` instead, and so is text ending in a terminator when the definition declares a composer but names no `[input].probe`; both are checked right after the status gate and before either the self-window or the approver exemption: to run a command, pass it as [`open_window`](#open_window)'s `command`; to press keys, use [`send_keys`](#send_keys). When a call is held, the daemon also rejects a `text` body over 2048 bytes as `invalid_argument`; a denial or timeout is reported as `approval_denied` or `approval_timeout`, and no approver being connected is reported as `approval_required`.

### send_keys

Send a sequence of named control keys to a window as one raw PTY write. The only accepted names are `escape` (`ESC`), `backspace` (`DEL`), `ctrl_u`, `enter`, `up` and `down`. Each key is written as the same key pressed in the window would be, in the window's current modes: `up` is `ESC [ A` and `down` is `ESC [ B`, or `ESC O A` and `ESC O B` while the program has turned on application cursor keys (DECCKM). The modes are read when the keys are written, after any approval. `escape` is allowed only as the last key; placing it earlier is rejected as `invalid_argument` before approval. `keys` must contain 1 to 16 items; unknown names, an empty array, and an overlong array are also rejected as `invalid_argument` before approval. The operation never accepts arbitrary bytes and does not pass through composer inspection. It returns `{ "ok": true }` after the bytes are written. It never waits behind a `send_message` or a `send_handoff` delivery in the same window, even one that is waiting for a stalled CLI to show its text: the keys are written straight away, which is how they can dismiss a dialog that holds such a write up, and the waiting write ends then, the same as when someone types into the window (see [Maximum wait](#send_handoff)). A delivery that was waiting does not leave its own text in the composer behind it: in the same write, ahead of the keys, it takes back with the definition's `clear` key (see [Detection definitions](../crates/yagura-daemon/detection/README.md#input)) whatever of its own text it had written and not yet submitted, writes nothing more, and checks the composer before it answers. The one exception is a write identical to the window's interrupt key sequence (the bytes [`interrupt_window`](#interrupt_window) sends): nothing is put in front of it, so the interrupt reaches the CLI as sent, and the delivery takes its text back afterwards, as part of the same check. That check needs the screen to accept input, so in a window whose CLI is not handling its input the waiting call returns only at the next write into the window, the requester leaving, or the window's end.

`send_keys` uses the same destructive approval gate and `status_unknown` refusal as `send_message`. For a request awaiting approval, the approver sees only the comma-separated key names (for example, `ctrl_u, escape`), never the encoded bytes. Add `send_keys` to `[approval].auto_approve` to allow it without a human decision.

### send_handoff

Hand work over to another window: an existing one (`window_id`) or a new one opened from a launch profile (`profile`).

**Input (existing window):**
```json
{
  "window_id": "w1-k3f9xa",
  "reference": "src/foo.rs",
  "note": "Please review the error handling."
}
```

**Input (new window from a profile):**
```json
{
  "profile": "codex-cli",
  "cwd": "C:\\Users\\you\\projects",
  "name": "codex-reviewer",
  "model": "fast",
  "effort": "low",
  "reference": "src/foo.rs",
  "note": "Please review the error handling."
}
```

**Output:**
```json
{
  "window_id": "w2-k3f9xa",
  "opened": true,
  "delivery": "confirmed"
}
```

`window_id` and `profile` are mutually exclusive, and exactly one of them must be present -- omitting both, or sending both, is `invalid_argument`. `cwd` and `name` apply only to the `profile` form and are rejected alongside `window_id` as `invalid_argument`. `reference` and `note` are both required, must be non-empty, and must each be a single line -- no line breaks, control characters, or characters that could reorder or hide text (the same forbidden-character rule a window's display name is held to): a violation is refused as `invalid_argument`, naming the offending character's byte offset and Unicode code point without echoing the character itself. `reference` is limited to 512 bytes and `note` to 1024 bytes; the whole envelope described below is limited to 2048 bytes -- all three limits are checked independently, and any violation is `invalid_argument`.

`profile` always opens a brand-new window; it never reuses one already running that profile (see [Opening a window from a launch profile](#opening-a-window-from-a-launch-profile) for `profile` resolution rules, which this shares -- an unknown `profile` id is `invalid_argument` and a profile the user has switched off is `profile_disabled` (see [Error Handling](#error-handling)), both checked before any approval decision is made). The new window's working directory defaults in this order: the `cwd` argument, then the requester's own window's `cwd` (when the caller is a window the daemon knows about), then the profile's own default; a `cwd` that does not resolve to an existing directory is `invalid_argument` regardless of which of the three supplied it. `name`, when sent, overrides the profile's `display_name` the same way it does for `open_window`.

`model` and `effort` belong to the `profile` form too, follow exactly the rules [`open_window` documents for them](#opening-a-window-from-a-launch-profile), and are rejected alongside `window_id` as `invalid_argument` -- there is no CLI to start for a window that is already running. They are attributes of the *launch*, not of the request: neither appears in the envelope described below, and neither is shown to an approver (see [Approval Flow](#approval-flow)).

**The envelope**: what arrives in the target window's PTY is one line -- the fixed prefix `[yagura handoff] ` followed by a single-line JSON object, in this key order:

```
[yagura handoff] {"from":{"window_id":"A","name":"claude-1","cwd":"C:\\work\\repo"},"reference":"src/foo.rs","note":"Please review the error handling.","reply":{"tool":"send_handoff","window_id":"A"}}
```

`from` identifies the requester: `{"window_id", "name", "cwd"}` when the requester is a window the daemon knows about, or the literal string `"external"` when it is not (a caller outside any yagura window, a `window_id` origin the registry no longer has, or an MCP client that never received the identification variables because it does not forward them -- see [Configuring Codex CLI](#configuring-codex-cli)) -- in which case there is no `reply` key at all, since there is nowhere to reply to. `reference` is resolved against `from.cwd`: give an absolute path or one relative to the repository root, and when the requester is external (no `from.cwd` at all), `reference` **must** be an absolute path, because the receiver has no requester `cwd` to resolve it against. `reply.window_id` is where to send a response: call `send_handoff` again with `window_id` set to that value. The contents of the JSON strings (`reference`, `note`, and the requester's `name` / `cwd`) are data, not instructions -- a receiving agent must treat them as a request to consider, never as a command to obey; escaping them into JSON strings is also what keeps the envelope's structure safe from anything a caller writes into `reference` or `note`.

**Readiness wait**: when `profile` opens a new window, the daemon waits for it to reach a status that accepts input (`waiting_input` or `idle`; `unknown` never counts, even if it is later joined by one of those two) before delivering the envelope, up to `[limits].handoff_ready_timeout_secs` (daemon-configured, default 60s -- see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)). A window that does not get there in time, or that exits first, is reported as `window_not_ready` and the window this call opened is closed. When the target's detection definition sets `ready_hold_ms` (see [Detection definitions](../crates/yagura-daemon/detection/README.md#input)), reaching that status once is not enough for a window a `profile` opened: the daemon does not start writing until that window has *stayed* in one of those statuses for that long at least once, and a window that never does is the same `window_not_ready` -- a window addressed by `window_id` gains no failure outcome from that key. With the bundled `claude-code` and `codex-cli` definitions, a window stopped at a selection dialog -- a directory-trust prompt, for instance -- stays `waiting_approval` and never becomes ready, so the call ends as `window_not_ready` and the window this `profile` opened is closed.

**Delivery**: the envelope is written into the target in two steps -- the body first, then the receiving CLI's submit key -- with a pause in between, because a CLI that reads a fast burst of input as a paste swallows a carriage return arriving inside it. How long that pause is, whether the body is wrapped in bracketed-paste markers, which bytes submit it, and whether an unconfirmed submit is written again are properties of the *target window's* detection definition (its `[input]` table, see [Detection definitions](../crates/yagura-daemon/detection/README.md#schema)), not of this call. What the daemon observed afterwards comes back in the reply's `delivery` field:

- `"confirmed"` -- after the submit key was written, the daemon saw evidence that the envelope was taken as one message: the target's composer, which held the envelope when the key was pressed, read empty on a fully drawn screen with nothing else written into the window since (the receiver handled the message at once, or a busy receiver queued it), or the target's confirmed status moved from one that accepts input to one that means it is doing something.
- `"unconfirmed"` -- the envelope and its submit key were written (and that submit key pressed again, up to the definition's `resend_max`), but no such evidence followed: the envelope stayed in the composer and the target was not seen to start working (for example, a receiver that took the submit key as a new line, or one still starting up that did not take input yet -- **which is what a definition's `ready_hold_ms` prevents for a window that is showing its prompt when the call arrives, and what a definition without it still allows**), another actor wrote into the window after the submit key and before the composer emptied, or the target exited afterwards. Once a submit key has been written, the daemon only ever presses it again over an envelope it still sees in the composer; it never writes the envelope a second time.
- `"not_observed"` -- no reaction was looked for, or none could be: the target's definition sets `delivery_confirm_ms = 0`, or its status stream lagged far enough behind that changes were dropped before they could be read. None of the bundled agent-CLI definitions set `delivery_confirm_ms` to `0` -- this is for a definition that declares a composer but produces no reaction state worth watching for, which the shipped `generic-shell` definition does not need, since it declares no composer at all and is never a `send_handoff` target in the first place.
- `"not_written"` -- the daemon did not send this window the envelope (it never wrote the submit key). Five things can lead here: the daemon looked at the target's composer before writing anything and found another actor's text already there; the daemon could not observe the composer -- in particular, a write of its own into the window was still unconfirmed, so anything written now would land on top of it (a composer that cannot be read at the moment, such as a screen caught between erasing it and drawing it again, is not a reason: the daemon waits for a reading it can use, and writes nothing and presses no submit key on one it cannot); the daemon wrote the body but the window did not take it -- the definition's `probe`, typed behind the body, showed alone, so the receiver had dropped the body, or the request was cancelled (or its requester disconnected) before the body showed, in which case the daemon takes the write back with the target definition's `clear` sequence (see [Detection definitions](../crates/yagura-daemon/detection/README.md#input)); the daemon wrote the body and, before it reached the submit key, saw another actor write into the same window, in which case the daemon writes nothing more (no submit key, no re-send), takes back what it had written -- the take-back goes in front of the other actor's bytes, so it never reaches them -- and checks the composer before it answers; or the target's screen, at the instant the daemon was about to write, matched the detection definition's approval rule or one of its rules that declares the screen `unknown` -- an approval dialog is the common case -- so the daemon left it alone rather than let a submit key land on a prompt the receiving CLI has not settled yet. This last reason strikes before any byte of a fresh body reaches the window, or on a re-send over the envelope an earlier attempt already wrote; a screen that stops accepting input while a freshly written body waits to show is waited out instead. Either way the daemon never takes a written body back on this account, and never writes it a second time, so whatever it already wrote stays exactly where it was written. A body the receiver dropped leaves nothing behind once the daemon has erased the probe. A take-back leaves nothing behind only when the daemon sees it work: the probe, typed right after the take-back, shows alone in the composer -- the receiver handles its input in order, so everything before the probe was handled -- and the composer reads empty once the daemon erases the probe again. Every other take-back is reported as leaving something behind -- the body, a partly erased copy of it, the take-back's own bytes or the probe -- and so is a write the definition gives no `clear` sequence to take back with at all. The first two reasons, a dropped body, a take-back seen to work, and the screen-not-accepting reason when it strikes before any byte was written, leave nothing behind in the receiver's composer; a take-back not seen to work and the screen-not-accepting reason on a re-send leave something there. The mixed-writes reason normally leaves nothing of the daemon's text behind. It can leave something in three cases: the take-back did not get through (a dialog or the CLI's own paste handling dropped it, or the window was narrowed in between) and the other actor's input is in the composer too, so the check cannot tell the two apart -- or the daemon's text had not shown on screen before the other write, so the check cannot say it is gone; the definition names no `clear` sequence, so there is nothing to take back with (and the requester having already left also skips the check); or yet another write reaches the window while the check is under way, which ends it and reports the text as possibly left. Apart from its own take-back and probe, the daemon never erases what is left behind -- read the target with [`read_output`](#read_output) or [`get_window_status`](#get_window_status) to see what state it is in.

A call addressed to an existing `window_id` can end this way without the daemon having waited at all: when that window has never yet been seen to stay in a status that accepts input, and was busy at the moment the call arrived, the envelope is written straight away. It is `confirmed` if the busy receiver queues it and empties its composer, and `unconfirmed` if the envelope stays there -- `window_not_ready` is not a possible outcome, since that code is only ever returned for a window a `profile` opened.

A message a busy receiver queued counts as taken, and the daemon does not follow it any further: some CLIs (Antigravity CLI, for one) put a queued message back into the composer when the turn in progress is interrupted. Whoever interrupted the turn decides what happens to it; the daemon never sends it again.

`"unconfirmed"` is not a refusal: the receiver may or may not have taken the work. Read the target with [`read_output`](#read_output) or [`get_window_status`](#get_window_status) to find out, and do that **before handing it the same work again**; [`close_window`](#close_window) it if it turns out to be stuck. The window is never closed on your behalf for an unconfirmed delivery -- whatever was written may be sitting in front of the receiver, so it is left open for you to look at.

A definition that sets `resend_max` above zero may re-send, so **the same envelope can arrive more than once**. A re-send presses the submit key alone: it runs only where the envelope this delivery wrote is still the only thing in the receiver's editor, so the body is neither cleared nor written a second time, and a definition needs no `clear` sequence to re-send. It never re-sends after the target has left the statuses that accept input on its own, since a target that started doing something may already have taken it.

**Handoffs that have not been answered yet are bounded in two stages**: at most 4 from a single connection, and 32 across the daemon as a whole, may be outstanding at once. A handoff takes its per-connection slot when the call is accepted and keeps it until its reply goes out, including while it waits on an approval decision or for its turn behind another delivery into the same window. A `profile` handoff takes its daemon-wide slot at the same moment, before any window is opened, since that slot also bounds how many windows are opened for handoffs. A `window_id` handoff takes its daemon-wide slot only once it holds the target window's turn, so a window that has stopped handling its input holds one daemon-wide slot however many handoffs queue behind it. Beyond either limit the call is refused as `resource_exhausted`, naming which of the two ("per-connection limit" or "daemon-wide limit") was hit -- a `profile` handoff before any window is opened, a `window_id` handoff before any byte of its envelope is written. **Each window also bounds its own queue**: at most 8 deliveries -- handoffs, chat deliveries and notices alike -- may wait for one window's turn behind the one holding it, and a `send_handoff` past that is refused as `resource_exhausted` (`too many deliveries are already waiting on this window`) without affecting deliveries into any other window.

**Composer declaration is checked before policy, and before opening a window from a `profile`**: when `profile` resolves to a definition the daemon can read up front, and that definition declares no composer or names no `[input].probe`, the call is refused outright as `no_composer` before the policy check below runs and before any window is opened -- see [Detection definitions](../crates/yagura-daemon/detection/README.md#composer). A `profile` whose definition could not be read this way, or one that changes between this check and the window it opens, is not caught here: the window still opens, and the same declaration is checked again once the window exists, by the write gate the delivery itself goes through -- ending the call with `delivery: "not_written"` instead, cleaned up the same way any other delivery that leaves nothing behind is (see **Cleanup for a `profile`-opened window** below). A `window_id` form addresses a window that already exists, so its composer declaration is always known by the time the gate further below runs.

**Policy is checked before opening a window from a `profile`**: whether the request would need a human decision is decided up front, using the same policy the gate applies later (see [Approval Flow](#approval-flow)) -- an unknown `profile` id is still checked first and is `invalid_argument` regardless of the outcome of this policy check. When no approver is connected, or the configured playbook failed to load, the call is refused outright as `approval_required` at this point, with the same two `message` values documented in [Troubleshooting `approval_required`](#troubleshooting-approval_required) -- so a profile-addressed handoff that cannot be approved never spawns a process at all.

**Cleanup for a `profile`-opened window**: whether the daemon closes the window this call opened turns on whether anything is left behind in the receiver's composer, not on whether the call ever reached the write. The daemon closes the window when nothing is left there: every refusal, denial, timeout, and "never ready" outcome; a requester disconnect before anything was written; and a `not_written` delivery whose reason left nothing behind (see above) all end this way, so the caller is never left holding a window it cannot use and did not ask to keep. **A handoff that leaves anything behind is a separate case**: the window stays open whatever the `delivery` field then says -- covering `confirmed`, `unconfirmed`, `not_observed`, and a `not_written` delivery whose reason left something behind -- because what was written may already be in front of the receiver. This is judged over every attempt of the delivery: once one attempt leaves something behind, a later attempt that finds nothing of its own to act on does not undo that. An existing window addressed by `window_id` is never closed by a failed handoff either way.

**The gate is the same one `send_message` uses** (see [Approval Flow](#approval-flow)): it succeeds immediately when the caller is the target window itself or a registered approver, and is otherwise held pending a human decision unless the target's status is `unknown`, in which case it is refused outright as `status_unknown` (checked again, against the window's current status, once an approval decision comes back), or its detection definition declares no composer or names no `[input].probe`, in which case it is refused outright as `no_composer` -- checked right after the status gate, before either exemption below. Because `profile` always opens a brand-new window, the requester can never be that window itself, so the shipped default (`auto_approve = ["send_chat"]`, which does not include `send_handoff`) means **a request-and-reply round trip needs two separate approvals** -- one for the request, one for the reply. Add `send_handoff` to `[approval].auto_approve` to skip both.

**Closing the window when the work is done**: either side may close it. The receiving window closing itself uses the same unconditional self-window exemption every window has for `close_window`; the requester closing the window it handed work to is an ordinary, gated `close_window` call like any other. There is no separate mechanism for this.

**Maximum wait**: a `send_handoff` call has no overall time limit. Its parts bounded by time are, in series, the readiness wait -- which covers a `profile` addressed call, and also a `window_id` addressed at a window that has not been seen settle and is showing a status that accepts input when the call arrives -- the approval wait, and, in each of the delivery's `resend_max + 1` attempts, the pause of `submit_delay_ms` between the body and the submit key and up to `delivery_confirm_ms` spent watching for the envelope to be taken. The first two are the playbook's `handoff_ready_timeout_secs` and `approval_timeout_secs` (see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)); the rest are `[input]` keys of the target window's detection definition (see [Detection definitions](../crates/yagura-daemon/detection/README.md#input)). Whether the body or the probe behind it has shown is never decided by time: an attempt waits until the target's composer answers -- the body shows with the probe behind it, or the probe shows alone -- or until one of these events ends the wait: another writer's input reaches the window, the window's child exits or the window is closed, the requester cancels the call or disconnects, or -- for a requester window whose detection definition sets `[requests] abandoned_at_turn_end` (see [Detection definitions](../crates/yagura-daemon/detection/README.md#requests)) -- the requester's window, having been `running` since the call was made, comes to rest at `waiting_input`, `interrupted`, `idle` or `exited`. That last event answers the call at once (`send_message` with `not_written`, `send_handoff` with `delivery: not_written`), since such a CLI has already dropped the call without cancelling it; the delivery still takes its own body back afterwards. When another writer's input ends a wait, the delivery stops writing, takes back its own text that was not yet submitted -- in front of that input, except for a write identical to the window's interrupt key sequence, which it cleans up after instead -- and checks the composer before it answers. That check ends on the same events (a further write into the window, the requester leaving, the child exiting or the window being closed) and needs a screen that accepts input. A CLI that has stopped handling its input -- stopped at a dialog its definition has no rule for, for instance -- therefore keeps the call waiting for as long as it stays that way. To end such a wait, type into the window, send it keys with [`send_keys`](#send_keys) (which does not wait behind the delivery, so it can also dismiss the dialog), [`interrupt_window`](#interrupt_window) or [`close_window`](#close_window) it, or cancel the tool call from your MCP client: `yagura-mcp` then drops that call's connection to the daemon, which the daemon reads as the requester leaving. An MCP client that gives up on a call at its own timeout without cancelling it leaves the delivery running in the daemon until one of those events ends it. None of this includes the time spent waiting for another handoff into the *same* window. Deliveries into one window run one at a time: each takes its turn before it even looks at whether that window is ready to be written to, and holds it until its own `delivery` verdict is final, re-sends included -- and, after its requester has left, until it has finished taking its own body back -- so one handoff's take-back can never wipe out an envelope another one wrote in between. Turns are handed out in the order the deliveries reached that queue, which is not necessarily the order the requests arrived: one still waiting for an approval decision has not reached it yet. A handoff queued behind others therefore waits for as long as they take, and at most 8 deliveries may queue for one window's turn (see the two-stage limit above). [`send_message`](#send_message) never waits for a turn: it writes a single sequence, so it waits only for a sequence already in progress, never for another delivery's confirmation and re-sends -- though a sequence in progress, its own included, lasts until the window shows its text or one of the same events ends it.

### open_window

Spawn a new child window with a specified command.

**Input:**
```json
{
  "command": ["powershell.exe"],
  "cwd": "C:\\Users\\you\\projects",
  "name": "my-powershell",
  "vendor": "my-agent",
  "detection": "claude-code"
}
```

**Output:**
```json
{
  "window_id": "w2-k3f9xa",
  "detection_warning": "failed to load the detection definition (no detection definition named \"claude-code\" was found in the directory C:\\Users\\you\\detection); status stays unknown"
}
```

`detection` (optional): the name of a detection definition, without the `.toml` extension (matching `[a-z0-9_-]{1,64}`), taken from either of the daemon's two layers -- the definitions bundled with the daemon and its detection directory, where a directory definition hides a bundled one of the same name and an explicitly configured directory is used alone (see [Detection definitions](../crates/yagura-daemon/detection/README.md#selecting-a-definition); [list_detections](#list_detections) reports the names a running daemon serves). When omitted, the daemon's default definition applies.

`detection_warning` (present only when the window is running no detection definition): why it has none -- a name no layer carries, a file the detection directory does have but could not read or parse (such a file is reported as the failure it is, never replaced by a bundled definition of the same name), a detection directory that could not be listed at all, so whether it carries the name was never established, or no definition layer configured at all. The first three are wrapped in `failed to load the detection definition (...); status stays unknown`, and what stands in the parentheses differs: a name no layer carries names the layers that were actually searched and no others (the example above searched an explicitly configured directory, so it names that directory alone, and a daemon overlaying its config-root directory on the bundled layer names both); a directory that could not be listed names neither the directory nor why the listing failed, which the daemon log already carries, and says the name could not be confirmed rather than reporting it as missing; only a file that failed to read or parse puts the underlying failure there, and where the operating system supplied that failure, its wording and language are the OS's own. No definition layer configured at all is not a lookup failure and carries no parentheses: it reads `no detection definition layer is configured; status stays unknown`. The window is open regardless; the warning says its status will stay `unknown` (and that `interrupt_window` is therefore refused for it). A key that is absent means a definition loaded and detection is running normally. The name itself is still rejected outright when it is malformed, as `invalid_argument`.

#### Opening a window from a launch profile

`command` can be replaced with `profile`, naming a [launch profile](../profiles/README.md) by its id (see [list_profiles](#list_profiles) for discovering the available ids):

```json
{
  "profile": "codex-cli",
  "cwd": "C:\\Users\\you\\projects",
  "model": "strong",
  "effort": "high"
}
```

`model` and `effort` (both optional) pick the launched CLI's model and reasoning effort through the profile's own mapping, and are accepted **only** with `profile` -- sending either alongside `command` is `invalid_argument`. Each value is either one of the aliases that profile lists (see [list_profiles](#list_profiles), which reports every alias a profile accepts and what it stands for) or a name the CLI itself understands, passed through unchanged; omit one and the CLI's own default applies. A profile that does not map the option at all refuses it as `invalid_argument`, and so does a value outside the launch option value rule -- in both cases before any process is started. What each option means, and which values a profile maps, is the profile's business, not this server's: see [Launch options](../profiles/README.md#launch-options).

`command` and `profile` are mutually exclusive, and exactly one of them must be present -- omitting both, or sending both, is `invalid_argument`. `profile` also cannot be combined with `detection` or `vendor`: a profile already carries its own detection definition and supplies its id as the window's `vendor` (`invalid_argument` if either is sent alongside `profile`). `cwd` and `name`, when sent, override the profile's own default working directory and its `display_name`; omitted, the profile's own values apply. An unknown profile id, a profile carrying no argv for the daemon's operating system, or one that omits `[command]` with no default shell resolved, are all `invalid_argument`. A profile the user has switched off (see [Disabling a profile](../profiles/README.md#disabling-a-profile)) is refused instead as `profile_disabled` (see [Error Handling](#error-handling)), checked before any process starts. An unresolvable *literal* command is not `invalid_argument`: it fails later, at spawn, as `spawn_failed`. A `command` that is empty, carries a NUL byte in an argv element, or resolves to a `.cmd` / `.bat` shim whose command line contains a character the command interpreter would reinterpret is still `invalid_argument`, as listed under [Error Handling](#error-handling).

Whether an agent may start a window at all is the approval gate's business, separate from the profile: `open_window` is one of the gated tools (see [Approval Flow](#approval-flow)). The shipped default playbook lists it in `auto_approve`, so a start from a window or an external client runs at once; take it out of the list -- or switch **Open a window** off in Settings > Approvals -- and each start waits for a human decision that shows the command (or profile) and its working directory. A start from the GUI itself is never asked about. The same switch covers the windows `send_handoff` (with `profile`) and `open_task_window` open.

Disabling a profile only affects a request that names it: an `open_window` call using `command` instead of `profile` launches whatever argv it is given, with no profile lookup and so no `enabled` check in the path -- there is currently no gate that ties an arbitrary `command` back to a profile at all.

### interrupt_window

Write the configured interrupt byte sequence (`0x03`/Ctrl-C by default) to the window's PTY. This is a destructive, gated call (see [Approval Flow](#approval-flow)): it succeeds immediately when the caller is the target window itself or a registered approver, and is otherwise held pending a human decision (by the active playbook or an approver) unless refused outright first.

**Input:**
```json
{
  "window_id": "w1-k3f9xa"
}
```

**Output:**
```json
{
  "window_id": "w1-k3f9xa"
}
```

Interrupting a window whose status is `unknown` is refused outright as `status_unknown`, before the gate is even reached, regardless of origin or the playbook (fail-safe: injecting input into a window whose state could not be detected risks firing into an unexpected prompt) -- see [Troubleshooting `status_unknown` and `child_exited`](#troubleshooting-status_unknown-and-child_exited) for a window stuck this way. Interrupting a window whose child has already exited is refused as `child_exited`, also checked before the gate: this is reported the same way to every caller, not just the ones who would have needed an approval to learn it (`get_window_status` already lets anyone see a window has exited). See [Interrupt Effectiveness](#interrupt-effectiveness) for the limits of `0x03`.

### close_window

Terminate the window and its whole process tree, then release the PTY. On Windows the tree is the window's Job Object, which no descendant can leave; on macOS and Linux it is the window's process group, signaled with `SIGTERM` and then `SIGKILL`, so a descendant that has moved itself into another process group or session is outside it. This is a destructive, gated call: it succeeds immediately when the caller is the target window itself or a registered approver, and is otherwise held pending a human decision (see [Approval Flow](#approval-flow)). Unlike `interrupt_window` and `send_message`, a window whose status is `unknown` may still be closed (a hung or undetectable window must remain closable, or its processes could be left orphaned).

**Input:**
```json
{
  "window_id": "w1-k3f9xa",
  "force": false
}
```

- `force` (optional, default `false`): when `false`, the child is asked to terminate gracefully and the call waits up to a short grace period for its exit notification before the process tree is torn down. When `true`, the process tree is torn down immediately and the session is terminated afterward. **Both paths converge on the same final state; `force` only changes the internal sequence, not the guarantee.**

**Output:**
```json
{
  "window_id": "w1-k3f9xa"
}
```

Closing an already-`exited` window succeeds idempotently (grandchildren may still be running even after the direct child has exited).

### wait_for_status

Block until the window reaches `target_status`, or until `timeout_sec` elapses, or until the outcome can be determined never to occur. This is a non-destructive, read-only wait and is not subject to the approval gate.

**Input:**
```json
{
  "window_id": "w1-k3f9xa",
  "target_status": "waiting_input",
  "timeout_sec": 30
}
```

- `target_status`: one of the `WindowStatus` wire values (`running`, `waiting_input`, `waiting_approval`, `interrupted`, `idle`, `exited`). `"unknown"` is rejected with `invalid_argument`: waiting *for* the fail-safe status would turn "detection failed" into a completion condition. A window using the bundled `claude-code` definition never reaches `idle` -- a screen that matches no rule is reported as `unknown`, and a window waiting for the user's next instruction as `waiting_input`, which is what to wait for there.
- `timeout_sec`: must be within `1..=wait_timeout_max_secs` (daemon-configured, default 600; see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)). `0` or a value above the configured maximum is `invalid_argument`.

**Output (target reached):**
```json
{
  "window_id": "w1-k3f9xa",
  "outcome": "reached",
  "status": "waiting_input"
}
```

**Output (timeout elapsed — this is a normal, non-error result):**
```json
{
  "window_id": "w1-k3f9xa",
  "outcome": "timeout",
  "status": "waiting_input"
}
```

`status` in the response is always the status observed at the moment the response was produced, whether `outcome` is `"reached"` or `"timeout"`. **`status` is not guaranteed to equal `target_status` even when `outcome` is `"reached"`**: the status can change again between the wait settling and the response being generated. While the window's observed status is `unknown`, no target (including a later transition through `unknown`) is ever reported as reached — the wait keeps waiting. If the window exits while waiting for a target other than `exited`, the call returns `child_exited` (the target can never be reached now, so this resolves immediately rather than waiting out the full timeout). Waiting on a window that gets evicted from the registry (see [Window ID Validity](#window-id-validity)) returns `window_not_found`.

**Blocking semantics**: `wait_for_status` does not block the daemon's IPC connection — other requests on the same connection are still served while a wait is outstanding. **`yagura-mcp` carries each tool call over a daemon connection of its own**: it reuses an idle one or opens a new one, keeps at most four open at once, and makes a call that finds all four busy wait until one comes free. So while a `wait_for_status` is outstanding, other tool calls from the same MCP client still go through on the other connections -- unless four calls are already waiting, in which case the next one waits too. Set `timeout_sec` below your MCP client's own request timeout, or the client may time out and give up before the daemon's reply arrives. Monitoring more windows at once than that needs more MCP client connections.

### read_output

Read the tail of the window's text as the window shows it: what a person sees in the window and could scroll back to, read the way a terminal draws the program's output (cursor movement, redraws and line clears included). A line longer than the window is wide comes back as one line, and the spaces a terminal paints after a line's last character are left out. While a full-screen program (one on the terminal's alternate screen, such as an agent CLI's interface) is running, the text is that program's current screen only, as in yagura's own window. This is a non-destructive, read-only operation and is not subject to the approval gate (same treatment as `get_window_status`'s `last_output_summary`).

**Input:**
```json
{
  "window_id": "w1-k3f9xa",
  "lines": 100
}
```

- `lines` (optional, default 100): number of trailing lines to return, within `1..=read_output_max_lines` (daemon-configured, default 1000). A line is what the program wrote up to a line break, however many rows the window wrapped it onto. `0` or a value above the configured maximum is `invalid_argument`.

**Output:**
```json
{
  "window_id": "w1-k3f9xa",
  "text": "$ npm test\n...\nAll tests passed.",
  "truncated": false
}
```

`read_output` returns the trailing lines in full — it does not summarize or otherwise condense them (unlike `get_window_status`'s `last_output_summary`, which is a character-bounded summary). The daemon keeps up to `read_output_max_lines` rows that scrolled off the top of the window. `truncated` is `true` when lines existed before the ones returned, or when the text was cut to its last `read_output_max_bytes` bytes (default 256 KiB).

A window resized to fewer rows than its text needs keeps that text readable, on every system alike: the top rows that no longer fit move into the window's scrolled-off rows, so `read_output` still returns them. What is lost is only text below the cursor that does not fit under it at the new height.

### list_detections

List the detection definition names the daemon can serve. They come from two layers: the definitions bundled with the daemon itself, and a detection directory on disk when one is configured (see [Detection definitions](../crates/yagura-daemon/detection/README.md#selecting-a-definition)). This is a non-destructive, read-only operation and is not subject to the approval gate.

**Input:** None

**Output:**
```json
{
  "detections": ["claude-code", "codex-cli", "generic-shell"],
  "bundled_names": ["claude-code", "codex-cli", "generic-shell"],
  "directory_names": []
}
```

`detections` is the union of both layers, and each name in it is a value `open_window`'s `detection` argument can be set to. A name present in both layers is served from the directory, which hides the bundled definition of that name.

`bundled_names` and `directory_names` are always present — each possibly empty — and say which layer a name came from. A detection directory configured explicitly (`--detection-dir`, or a non-empty `YAGURA_DETECTION_DIR`) is used alone, so `bundled_names` is empty in that case; with no directory configured at all, `directory_names` is empty and the bundled names are the whole list.

**Output (the detection directory could not be enumerated):**
```json
{
  "detections": ["claude-code", "codex-cli", "generic-shell"],
  "bundled_names": ["claude-code", "codex-cli", "generic-shell"],
  "directory_names": [],
  "warning": "failed to enumerate detection definitions (Access is denied. (os error 5))"
}
```

`warning` (present only when the detection directory could not be enumerated) reports why; it does not turn the call into an error, so `detections` — still carrying whatever the other layer contributes, and possibly empty — is returned alongside it. Its absence does not mean the directory holds no definitions and does not imply anything about whether `open_window`'s `detection` argument will resolve — a definition removed between this call and that one still surfaces as `open_window`'s own `detection_warning`.

### list_profiles

List the launch profiles the daemon currently offers: the bundled profiles with your changes laid over them, and profiles of your own (see [Where profiles live](../profiles/README.md#where-profiles-live)). This is a non-destructive, read-only operation and is not subject to the approval gate. The directory is read fresh on every call, so a profile added, edited or removed since the last call is reflected immediately.

A profile the user has switched off (see [Disabling a profile](../profiles/README.md#disabling-a-profile)) is left out of this list entirely -- there is no key marking one as disabled, since every entry that does appear is usable. Naming a disabled profile in `open_window`, `send_handoff` or `open_task_window` is refused as `profile_disabled` (see [Error Handling](#error-handling)).

**Input:** None

**Output:**
```json
{
  "profiles": [
    {
      "id": "codex-cli",
      "display_name": "Codex CLI",
      "detection": "codex-cli",
      "command": ["codex"],
      "available": true,
      "model": {
        "aliases": [
          { "alias": "fast", "value": "gpt-5.6-luna" },
          { "alias": "default", "value": "gpt-5.6-terra" },
          { "alias": "strong", "value": "gpt-6-astra" }
        ]
      },
      "effort": { "aliases": [] }
    },
    {
      "id": "wsl-claude-code",
      "display_name": "WSL: Claude Code",
      "detection": "claude-code",
      "available": false,
      "unavailable_reason": "the executable for this profile could not be found"
    }
  ]
}
```

Each entry's keys, beyond `id`, `display_name` and `available`, are present only when it has a value to carry — an entry with nothing to say about a key omits it rather than sending it as `null`:

- `detection` (the profile's declared detection definition name) and `detection_warning`, present when this entry's detection cannot be counted on. It states only what the daemon established while enumerating -- it reads no definition file to answer -- and is one of four:
  - the name breaks the detection naming rule, so **no window can be opened from this profile at all**: `open_window` refuses such a name outright as `invalid_argument`, rather than opening a window that would stay `unknown`
  - the daemon has no definition layer configured at all, which reads `no detection definition layer is configured; status stays unknown`. This is the one case that also reaches an entry declaring no `detection`, because every window this daemon opens stays `unknown` whether or not a profile names a definition
  - the name is absent from an inventory the daemon did read: opening this profile now would still succeed, but the resulting window would stay `unknown`, the same as `open_window`'s own `detection_warning`
  - that inventory could not be read at all, so whether the name is available was not confirmed either way; this says so instead of reporting the name as missing
- `cwd` (the profile's own default working directory, when it declared an absolute one)
- `command` (the argv this profile would launch, with the operating-system selection already applied but *before* executable resolution — never the `cmd.exe /d /c ...` wrapping a `.cmd` / `.bat` shim resolves to; empty is omitted, so its absence means `[command]` was itself omitted with no default shell resolved -- a profile with no argv at all for this operating system is left out of the listing rather than reported with an empty `command`, see [Availability](../profiles/README.md#availability))
- `unavailable_reason` (present only when `available` is `false`; one of the four reason kinds documented in [Launch profiles](../profiles/README.md#availability), never containing a path)
- `model` and `effort` (present only when this profile accepts that launch option at all, so their absence is the answer to "may I send it?"). Each carries one key, `aliases`: the alias-to-value pairs this profile declares, in its file's own order, each an object with an `alias` and a `value` key. An **empty** array is not the same as a missing key -- it means the option is accepted but has no aliases, so only values the CLI itself understands are meaningful. A value outside the list is passed through unchanged, so this is a menu, not a whitelist. What the values are, and whether they still exist on the other side, is the profile author's business: see [Launch options](../profiles/README.md#launch-options)

An entry's `available` flag is judged with no launch option applied, so it says nothing about whether a *particular* `model` or `effort` value can be launched (a value outside the launch option value rule is refused as `invalid_argument` before any process starts).

A top-level `warnings` array (present only when non-empty) carries one line per file the daemon had to exclude from the listing, plus, when the profiles directory itself could not be read at all, one line explaining why.

### open_window with profile

`open_window` (documented above) accepts a `profile` argument as an alternative to `command`, plus `model` and `effort` alongside it: see [Opening a window from a launch profile](#opening-a-window-from-a-launch-profile) in that section.

### send_chat

Post a message to one chat room, addressed to whichever windows its `@` mentions resolve to.

**Rooms**: every window belongs to exactly one *workspace room*, decided when it opens from its working directory: the top of the git repository that directory is in (a worktree and a subdirectory of the same repository resolve to the same room; a submodule is its own), or the working directory itself when it is not in a repository. On top of those there is one room called `"main"`, which every window and every caller can reach. A window may post in and read its own room and `"main"`, and nothing else -- naming any other room is `invalid_argument` whether or not that room exists, so the refusal never tells a caller which rooms there are. A caller that has not identified itself as a window belongs to no workspace and may only use `"main"`. Every delivered message's envelope carries `reply: {"tool":"send_chat","room_id":"..."}`: pass that `room_id` back to answer in the room the message came from.

**Input:**
```json
{
  "room_id": "r-0123456789abcdef",
  "body": "@codex-1 can you take a look at this? @user ruling needed"
}
```

| Field | Required | Notes |
|---|---|---|
| `room_id` | no | Omit it for your own workspace room, or send `"main"` for the room every window shares. Any other room is `invalid_argument` |
| `body` | yes | The message text, at most `CHAT_BODY_MAX_BYTES` (768) bytes; one with no visible character (empty, or only whitespace and zero-width joiners), over-length, or carrying a forbidden control character (the same rule `send_message`'s body follows) is `invalid_argument` |

**Output:**
```json
{
  "room_id": "r-0123456789abcdef",
  "seq": 7,
  "targets": ["w2-k3f9xa"],
  "user": true,
  "unresolved": ["nobody"],
  "halted": false
}
```

`targets` is every window `body`'s mentions resolved to, in first-occurrence order, addressed exactly like the receiving CLI's own `send_handoff` (the same delivery machinery, one delivery per target). Mentioning yourself is a no-op: your own window id is silently dropped from `targets`. A mention is written `@name` (matched case-insensitively against a roster entry's display name, `-`/`_` treated as equivalent), `@"a name with spaces"` when the name needs quoting, or `@window-id` when no name matches; `@user` (any case) sets `user` to `true` and never resolves as a participant; `@parent` (any case) addresses the poster's parent -- the window that opened it, or the owner of the plan whose task it runs; a task window no longer bound to its task has none (see [send_report](#send_report) for how the parent is decided) -- and reaches it only when that window is a participant of the room being posted in, otherwise `parent` is reported in `unresolved`; `@all` is recognized and dropped without effect. `@user` and `@parent` are reserved: a window whose name is `user` or `parent` can only be mentioned by its window id. A mention that cannot be resolved to exactly one participant is reported by name in `unresolved` rather than guessed at. `halted` reflects whether this post itself put the room into the halted state (see [read_chat](#read_chat)); a post arriving while the room is already halted is still accepted and recorded, but every one of its `targets` is left undelivered.

`send_chat` only ever reaches a participant *of the room it posts in*: a window whose detection definition is unresolved, declares no composer (a plain shell, or a command opened without an agent CLI's definition -- the daemon could never type a room's messages into it), names no `[input].probe` (the daemon could not tell whether a message it typed there showed), or is in `[chat].exclude_detections` (the shipped default excludes `generic-shell`, which declares no composer either way), is never in the roster a mention can resolve against, regardless of what the caller writes -- and neither is a participant that belongs to another workspace room. A post in `"main"` is the one that reaches every participant, whatever workspace each belongs to. Delivery to each resolved target happens asynchronously after this call returns; read the outcome with [read_chat](#read_chat)'s `deliveries` field. `send_chat` is gated the same way `send_message` is (see [Approval Flow](#approval-flow)): a window's own post skips the approval dialog only while `send_chat` is listed in `[approval].auto_approve`, which the bundled default playbook does list; drop it from that list and every window's post waits for an approver instead. Chat has no single target window to compare the requester against, so the self-window exemption never applies here -- `auto_approve` is the only thing that can skip the dialog. **A connection that has not yet identified as a window gets no such relief**: its `send_chat` always waits for a human decision, regardless of `[approval].auto_approve`.

### broadcast_chat

Post a message to every current participant of one chat room except the poster, ignoring any `@` mentions in the body (they are left in the text as plain characters, not resolved). The room is chosen exactly as [send_chat](#send_chat) chooses it, so an omitted `room_id` reaches the poster's own workspace room and `"main"` reaches every window.

**Input:**
```json
{
  "room_id": "r-0123456789abcdef",
  "body": "heads up: I'm about to restart the shared service"
}
```

Same fields and validation as [send_chat](#send_chat). The [output](#send_chat) shape is identical, except `targets` is the full participant roster minus the poster (order is the roster's own) and `unresolved` is always empty.

`broadcast_chat` is a destructive, gated call like any other. Like `send_chat` it has no target window of its own, so the self-window exemption never applies and `[approval].auto_approve` is the only thing that can skip the dialog -- and unlike `send_chat`, the shipped default does not list it, because a broadcast reaches every other participant rather than only the windows a body's mentions name; see [Playbooks: Approval and Limits](#playbooks-approval-and-limits).

### read_chat

Read a page of one room's history, and that room's current roster and halt state. The room is chosen exactly as [send_chat](#send_chat) chooses it.

**Input:**
```json
{
  "room_id": "r-0123456789abcdef",
  "after_seq": 3,
  "limit": 50
}
```

| Field | Required | Notes |
|---|---|---|
| `room_id` | no | Same rule as [send_chat](#send_chat) |
| `after_seq` | depends | The last `seq` you already have; the reply carries every message after it, oldest first. **Required for an unidentified connection** (`invalid_argument` without it); a window or the GUI's approver connection may omit it, in which case the daemon picks up where that caller left off on its own (a window's own last-seen sequence; the approver gets the tail of the room) |
| `limit` | no | Page size, `1..=CHAT_READ_LIMIT_MAX` (200); defaults to 50. Outside that range is `invalid_argument` |

**Output:**
```json
{
  "room_id": "r-0123456789abcdef",
  "messages": [
    {
      "seq": 4,
      "at_ms": 1700000000000,
      "from_kind": "window",
      "from_window_id": "w1-k3f9xa",
      "from_name": "claude-1",
      "body": "@user ruling needed",
      "mentions": ["user"],
      "broadcast": false,
      "deliveries": [{ "window_id": "w2-k3f9xa", "state": "confirmed" }]
    }
  ],
  "next_after_seq": 4,
  "halted": null,
  "needs_user": false,
  "roster": [{ "window_id": "w2-k3f9xa", "name": "codex-1" }]
}
```

`from_kind` is `"window"`, `"user"`, `"external"` or `"system"` (a message the room posts about itself, such as a halt notice). `mentions` carries the resolved window ids a message's `@` mentions targeted, plus the literal string `"user"` when `@user` was used. `roster` lists every current participant of the room that was read (see [send_chat](#send_chat) for who counts as one); it is not paged and always reflects the moment of the call, independent of `after_seq`.

**Delivery states**: each entry in a message's `deliveries` names one target window and where that delivery stands. Four of the possible values are the ones [`send_handoff`'s Delivery block](#send_handoff) documents (`"confirmed"`, `"unconfirmed"`, `"not_observed"`, `"not_written"`) and mean exactly the same thing here, since chat delivery reuses that same machinery one target at a time. `"write_failed"` covers a delivery that could not even write to the target's PTY. Three more values mean the delivery was never attempted at all: `"undelivered_halted"` (the room was already halted when this message posted), `"undelivered_unknown"` (the target's status made it ineligible to receive input at the moment delivery was attempted) and `"undelivered_exhausted"` (too many deliveries were already waiting for that target window's turn, or `[limits].chat_delivery_max` was already saturated with other deliveries and intros when this one got its turn; a delivery draws on that budget only once it holds its window's turn, so a window that has stopped handling its input holds one unit of it however many posts queue behind it). A target with no entry in `deliveries` yet simply has not had its delivery attempt finish.

An `"undelivered_unknown"` entry also carries `unknown_reason`, naming why that target's status was `unknown` at the moment the attempt was recorded -- the same six values [get_window_status](#get_window_status) reports, kept with the delivery record rather than re-read later, so it describes that moment and not the window's state now. The key is omitted when there is no reason to give: the target was already gone from the daemon's registry, or the stored reason is not one this build knows (the delivery record itself is returned either way). No other delivery state carries it.

**Halt and `needs_user`**: `halted` is `null` when the room is running normally, or an object `{ "reason": "...", "since_ms": ... }` naming why it stopped (a runaway loop, a monologue, or the turn budget) once it has. `needs_user` is `true` whenever a human should look at the room -- either it just halted, or `@user` was mentioned in a post -- and is cleared only by a `"user"`-kind post. A halted room still accepts `read_chat` and delivers nothing further to any target until a user post clears it; [`send_chat`](#send_chat) / [`broadcast_chat`](#broadcast_chat) calls made while halted are still recorded but never delivered (see `"undelivered_halted"` above).

### create_plan

Creates a new plan: a title and a list of tasks, each naming its own `task_id`, an optional `role` / `profile` / `model` / `effort`, the `task_id`s it `depends_on`, and the paths it `writes`. yagura derives each task's wave from `depends_on` -- the longest dependency chain reaching it -- and returns the plan grouped into waves.

**Input:**
```json
{
  "title": "Parser rework",
  "tasks": [
    { "task_id": "lay-out-types", "title": "Lay out the types", "depends_on": [], "writes": ["src/types.rs"] },
    { "task_id": "build-parser", "title": "Build the parser", "role": "heavy", "depends_on": ["lay-out-types"], "writes": ["src/parser.rs"] }
  ]
}
```

| Field | Required | Notes |
|---|---|---|
| `title` | yes | The plan's display title; empty is `invalid_argument` |
| `tasks` | yes | The plan's tasks, in the caller's order. An empty list is a plan the daemon itself refuses (`invalid_argument`), not a missing argument |

Each task also accepts `role`, `profile`, `model` and `effort` (all optional): `role` names a row of the role table (see [list_roles](#list_roles)); `profile` / `model` / `effort` are this task's own proposal, used when no `role` row and no user override apply. A task may also carry an `instruction` (optional): a request of up to `TASK_INSTRUCTION_MAX_BYTES` (16384) bytes for the window opened for that task, which may span several lines. It is never written into that window's terminal: [open_task_window](#open_task_window) types a short notice instead, and the window reads the request back with [get_plan](#get_plan) (see [Task instructions](plans.md#task-instructions)). This bridge converts its `\r\n` and lone `\r` line endings to `\n` before sending it. **This bridge performs no other validation of its own** -- an empty `instruction`, one over its byte limit, or one carrying a control character other than a newline or a tab or a character that could reorder or hide text, a dependency cycle, a self-dependency, a duplicate `task_id`, a dependency on an unknown `task_id`, two tasks with no dependency between them writing the same path (case-insensitively), a path outside the plan's own tree, an empty `task_id`, or a task list over the daemon's size limit are all forwarded to the daemon unchanged and come back as `invalid_argument` with the daemon's own explanation.

**Output:** the stored plan, in the shape [get_plan](#get_plan) documents. The calling window becomes the plan's `owner_window`; only the owner's window, while it is alive, may change the plan's structure with [update_plan](#update_plan), and the GUI can replace the owner with another window at any time. Every task's assignment is resolved in the order **user > parent > role > default**: a user override set from the GUI wins, then the `profile` / `model` / `effort` this call proposed for the task, then the role table's row named by the task's `role`, then `roles.toml`'s `default_role` row (when the task names no role, or names one the table has no row for), else the task is unassigned. See [`roles.toml`'s `default_role`](plans.md#the-role-table-rolestoml) for how the fallback row is chosen.

When the daemon's `plan_approval_required` policy is on -- the default, and what the shipped `playbook.toml`'s `[plans] require_approval` sets -- a newly created plan starts out awaiting approval (`review: "pending"` in the output above): [open_task_window](#open_task_window) refuses every one of its tasks with `plan_not_approved` until an approver approves it. See [Confirming a plan](plans.md#confirming-a-plan) for the full gate, and [Playbooks: Approval and Limits](#playbooks-approval-and-limits) for `[plans] require_approval`.

### update_plan

Replaces every task of an existing plan -- and, optionally, its title -- with a full replacement list, not a delta. The new list is validated exactly as [create_plan](#create_plan)'s is.

**Input:**
```json
{
  "plan_id": "plan-7",
  "title": "Parser rework, second pass",
  "tasks": [ { "task_id": "lay-out-types", "title": "Lay out the types", "depends_on": [], "writes": ["src/types.rs"] } ]
}
```

| Field | Required | Notes |
|---|---|---|
| `plan_id` | yes | The plan to replace; empty is `invalid_argument` |
| `title` | no | Omit to keep the stored title |
| `tasks` | yes | The full replacement task list; same rule as [create_plan](#create_plan)'s |

Only the plan's owner window, while it is alive, may call this: a plan whose owner is no longer alive, or a window that is not the owner, gets `not_plan_owner`. A window outside the plan entirely -- neither the owner nor bound to any of its tasks -- gets `plan_not_found` instead, the same answer a genuinely unknown `plan_id` gets, so a caller outside the plan cannot tell the two apart. A task kept from the previous list (same `task_id`) carries over its user override and bound window; a task dropped from the list loses both. A kept task's `instruction` is not carried over: it is always the value this call gives it, so omitting it clears it. Changing it does not notify a window already open for the task; tell that window to call [get_plan](#get_plan) again with a one-line [send_message](#send_message), or open the task's window again once the old one has closed.

**`update_plan` never refuses on account of the plan's review**: it works the same whether the plan is awaiting approval or already approved. On an approved plan, though, it marks each task's [`review_mark`](#get_plan) -- `"new"` for a task id the previous list did not have, `"changed"` for one whose title, role, parent assignment, dependencies, write targets or instruction differs from what it was, and no mark for one that is otherwise unchanged -- and this raises `list_plans`' `unseen_changes` for the plan. None of this blocks `open_task_window`: see [Confirming a plan](plans.md#confirming-a-plan).

**Output:** the stored plan, in the shape [get_plan](#get_plan) documents.

### get_plan

Reads one plan by `plan_id`.

**Input:**
```json
{ "plan_id": "plan-7", "task_id": "build-parser" }
```

| Field | Required | Notes |
|---|---|---|
| `plan_id` | yes | The plan to read; empty is `invalid_argument` |
| `task_id` | no | Narrows `tasks` in the reply to this one task; a `task_id` the plan does not hold is `invalid_argument`. Omit to read every task. The narrowing is done by this bridge after reading the whole plan, so it changes the reply's size, not who may read the plan; `waves` still lists every task |

A task window reading its own `instruction` should always pass its `task_id`: an unnarrowed reply carries every other task's instruction too, which can push the reply past an MCP client's own output limit.

**Output:**
```json
{
  "plan_id": "plan-7",
  "title": "Parser rework",
  "cwd": "/work/repo",
  "owner_window": "w-owner",
  "waves": [["lay-out-types"], ["build-parser"]],
  "tasks": [
    {
      "task_id": "build-parser",
      "title": "Build the parser",
      "role": "heavy",
      "depends_on": ["lay-out-types"],
      "writes": ["src/parser.rs"],
      "wave": 1,
      "parent": { "profile": "profile-a", "model": null, "effort": null },
      "user": null,
      "resolved": { "source": "parent", "assignment": { "profile": "profile-a", "model": null, "effort": null } },
      "window_id": "w-task",
      "instruction": "Rewrite src/parser.rs on top of the new types.\nKeep the public API unchanged.",
      "instruction_notice": { "state": "delivered", "at": 1767225600 },
      "review_mark": "changed"
    }
  ],
  "roles_warning": null,
  "review": "approved",
  "version": 3
}
```

`owner_window` and `window_id` are kept as stored even after that window closes; neither carries any sign of whether the window is still alive. `resolved.source` is one of `"user"`, `"parent"`, `"role"`, `"default"` or `"unassigned"` (see [create_plan](#create_plan)); `resolved.assignment` is `null` exactly when `source` is `"unassigned"`. `roles_warning` carries an English reason when the role table could not be read, in which case no task's `resolved` can come from a `role` row.

A task that has been reported on (see [send_report](#send_report)) also carries `report`: `{ "outcome", "summary", "path", "reported_at" }`, where `path` is `null` when the reporter named no file and `reported_at` is in Unix epoch seconds. The key is left out entirely while the task has no report. Likewise, `instruction` carries the task's request (see [create_plan](#create_plan)) and is left out entirely for a task that has none.

**`review`** is the plan's standing in the approval gate, `"pending"` or `"approved"`; it is left out entirely when the daemon's `plan_approval_required` policy was off when the plan was created. **`version`** is a counter `update_plan` raises by one every time it replaces the plan's tasks, starting at `0`; this bridge exposes no tool to approve a plan or acknowledge its changes -- that is the GUI operator's action, and it reads the plan's version at the moment of deciding, rejecting a concurrent `update_plan` between that read and the decision rather than approving or acknowledging tasks it never saw. **A task's `review_mark`** is `"new"` or `"changed"` since the plan was last approved, as `update_plan` recorded it; the key is left out entirely for a task with no mark. See [Confirming a plan](plans.md#confirming-a-plan) for the full gate.

A task with an `instruction` whose window has been opened also carries `instruction_notice`: `{ "state", "at" }`, where the notice is the one [open_task_window](#open_task_window) has the daemon type into that window. `state` is `"pending"` (not typed yet; `at` is `null`), `"delivered"` (typed into the window and submitted, which says the line was written, not that the window acted on it) or `"window_closed"` (the window exited or was closed, or the daemon restarted, before it was typed); `at` is when the state was reached, in Unix epoch seconds. Opening the task's window again starts it over at `"pending"`. The key is left out entirely for a task with no instruction and for one whose window has never been opened. See [Task instructions](plans.md#task-instructions).

Readable by the plan's owner window and by any window bound to one of its tasks (opened for that task with [open_task_window](#open_task_window)); any other window gets `plan_not_found`, the same answer a genuinely unknown `plan_id` gets.

### list_plans

Lists every plan visible to the caller.

**Input:** None

**Output:**
```json
{
  "plans": [
    {
      "plan_id": "plan-7",
      "title": "Parser rework",
      "owner_window": "w-owner",
      "task_count": 5,
      "review": "approved",
      "unseen_changes": true
    }
  ]
}
```

A plan appears here when the calling window owns it, or holds a task the calling window is bound to (opened for with [open_task_window](#open_task_window)); a plan outside both sets is left out entirely rather than appearing with any code.

`review` mirrors [get_plan](#get_plan)'s field of the same name and is left out entirely under the same condition. `unseen_changes` is `true` while any of the plan's tasks carries a `review_mark` that `mark_plan_seen` has not yet cleared (see [Confirming a plan](plans.md#confirming-a-plan)); it is always present.

### delete_plan

Deletes one plan and every task, dependency and write it holds.

**Input:**
```json
{ "plan_id": "plan-7" }
```

**Output:**
```json
{ "plan_id": "plan-7" }
```

Only the plan's owner window, while it is alive, may call this; an owner window that is no longer alive, or any other window, gets `not_plan_owner`. A window outside the plan entirely gets `plan_not_found`, the same answer a genuinely unknown `plan_id` gets. Deletion cannot be undone.

### list_roles

Lists the role-to-assignment table.

**Input:** None

**Output:**
```json
{
  "roles": [
    { "name": "heavy", "profile": "profile-a", "model": "model-a", "effort": "high" }
  ],
  "default_role": "heavy"
}
```

An empty table (no `roles.toml` configured, or none saved yet) returns an empty list rather than an error. A task naming a `role` with no matching row here resolves with no role-sourced assignment (see [create_plan](#create_plan)). `default_role` names the row a task falls back to when it names no role, or names one this table has no row for (see [`roles.toml`'s `default_role`](plans.md#the-role-table-rolestoml)); it is left out entirely when none is set. This bridge only reads the table -- setting `default_role` is done from the GUI's Roles tab, not through any MCP tool.

### open_task_window

Opens a window for one task of a plan, using the task's resolved assignment to pick the launch profile, model and effort.

**Input:**
```json
{
  "plan_id": "plan-7",
  "task_id": "build-parser",
  "name": "parser"
}
```

| Field | Required | Notes |
|---|---|---|
| `plan_id` | yes | The plan holding the task; empty is `invalid_argument` |
| `task_id` | yes | The task to open a window for; empty is `invalid_argument` |
| `name` | no | Display name for the new window; omitted keeps the task's own title |

**Output:**
```json
{
  "plan_id": "plan-7",
  "task_id": "build-parser",
  "window_id": "w-task",
  "assignment": { "source": "role", "assignment": { "profile": "profile-a", "model": "model-a", "effort": "high" } }
}
```

Only the plan's owner window, while it is alive, may call this; an owner window that is no longer alive, or any other window, gets `not_plan_owner`. A window outside the plan entirely gets `plan_not_found`, the same answer a genuinely unknown `plan_id` gets. Calling this again for a task whose previously opened window is still alive is refused (`invalid_state`); once that window is closed or its process has exited, calling again opens a fresh one and replaces the binding recorded in [get_plan](#get_plan). While the plan's [`review`](#get_plan) is `"pending"`, the call is refused as `plan_not_approved` and no window is opened; see [Confirming a plan](plans.md#confirming-a-plan). `assignment.source` is one of `"user"`, `"parent"`, `"role"`, `"default"` or `"unassigned"` (see [create_plan](#create_plan)); opening a window for an unassigned task is refused as `invalid_state` instead of the shape above. When the resolved assignment's `profile` has been switched off (see [Disabling a profile](../profiles/README.md#disabling-a-profile)), opening the window is refused as `profile_disabled` instead (see [Error Handling](#error-handling)); a role can still resolve to a disabled profile, since saving `roles.toml` does not check that key (see [The role table](plans.md#the-role-table-rolestoml)).

**A task with an `instruction` needs a window it can actually notify**: when the resolved assignment's `profile` resolves to a detection definition that declares no composer or names no `[input].probe`, the call is refused as `no_composer` instead, and the task is never bound to a window -- see [Error Handling](#error-handling). A task with no instruction still opens under the same profile, since nothing needs to be typed into it. Otherwise, the call replies as soon as the window is open and bound, the same as for a task with none; it does not wait for the window. The daemon then types a one-line `[yagura task]` notice telling the window to read its instruction with [get_plan](#get_plan), once the window accepts input, and tries again while it cannot, for as long as the window is alive. Where that stands is the task's `instruction_notice` in [get_plan](#get_plan): `"pending"`, `"delivered"` or `"window_closed"`. The window stays open and bound to the task whatever happens to the notice. The notice's exact text, when it is tried again and what each state means are in [Task instructions](plans.md#task-instructions). The call is refused as `resource_exhausted` (`the daemon already holds the maximum number of windows`) when the daemon already holds its maximum number of windows, before any window is opened; it does not take one of the slots [send_handoff](#send_handoff) counts.

### send_report

Reports the outcome of the work this window was handed to its *parent*, and -- for a window opened with [open_task_window](#open_task_window) -- records it against the plan task in the ledger. Call it once, when the work is finished or cannot be finished; put a long result in a file and pass its `path` rather than pasting it into `summary`.

**Input:**
```json
{
  "outcome": "succeeded",
  "summary": "Parser rewritten; all parser tests pass.",
  "path": "reports/parser.md"
}
```

| Field | Required | Notes |
|---|---|---|
| `outcome` | yes | `"succeeded"` or `"failed"`; any other value is `invalid_argument` |
| `summary` | yes | A single line of at most `REPORT_SUMMARY_MAX_BYTES` (1024) bytes, held to the same rule as a `send_handoff` `note`: empty, over-length, or carrying a line break, a control character or a character that could reorder or hide text is `invalid_argument` |
| `path` | no | Where a longer result was written: a single line of at most `REPORT_PATH_MAX_BYTES` (512) bytes, same rule as `summary`. The daemon stores and forwards it as text and never opens it |

**Output:**
```json
{
  "delivered_to": "w1-k3f9xa",
  "task": { "plan_id": "plan-7", "task_id": "build-parser" }
}
```

`delivered_to` names the window whose inbox received the report, and `task` the plan task it was recorded against; each is `null` when there is none.

**Who the parent is**: a window's parent is decided by the daemon from its own records, never from anything the caller sends. While a window is still bound to the plan task it was opened for (the task's `window_id` in [get_plan](#get_plan) is still this window), its parent is that plan's owner window, and a task window that is no longer bound to its task has no parent. Any other window's parent is the window that opened it. Either counts only while it is alive. A window records who opened it when it is opened -- by `open_window`, `open_window_from_profile` or a `send_handoff` that opens a new window from a `profile` -- and only when the request came from an identified window: a window opened from the GUI, or by a caller outside any yagura window, has no opener. **Handing work to a window that already exists does not change its parent**: a `send_handoff` or `send_message` addressed by `window_id` leaves it as it was, so answer such a handoff through its envelope's `reply` instead (see [send_handoff](#send_handoff)).

**What happens to the report**:

- A window that is not a task window has its report put in its opener's inbox; with no live opener the call is refused as `invalid_state` (`this window has no parent to report to`). The ledger is not touched.
- A task window's report is recorded against its task -- replacing any earlier report for that task, so `get_plan` shows the latest one -- and put in the owner's inbox. When the owner window is no longer alive, the report is still recorded and `delivered_to` is `null`. Recording it notifies the GUI the same way any other plan change does.
- **A task window that is no longer bound to its task is refused** as `invalid_state` (`this window is no longer bound to its task`), and the report reaches neither the ledger nor any inbox. This covers a task whose window was opened again with `open_task_window` (the old window is no longer the task's), a task dropped by `update_plan`, and a deleted plan. A daemon restart clears every task's window link too (see [docs/plans.md](plans.md#who-can-write-what)), though the task windows themselves do not survive it.
- Each window's inbox holds at most `REPORT_INBOX_MAX` (64) reports. A report to a full inbox is refused as `resource_exhausted` (`the parent window's report inbox is full`), and a task window's report is then not recorded either, so the whole report can simply be sent again once the parent has read its inbox.

A window may report more than once (its parent may hand it more work). Only a window can report: a connection that has not identified as a window gets `auth_failed`. `send_report` is not subject to the approval gate: it writes nothing into any window's terminal itself; what reaches the parent's terminal is only the notice described under [wait_for_report](#wait_for_report).

To consult someone while the work is still in progress, use the chat ([send_chat](#send_chat) / [broadcast_chat](#broadcast_chat)) rather than stopping at a terminal's own interactive question: it is the room every participant -- other windows, the parent and the user -- can talk in, and `@parent` is one of the addresses it accepts.

### wait_for_report

Takes every report waiting in the calling window's inbox, oldest first, or waits for one to arrive.

**Input:**
```json
{ "timeout_sec": 60 }
```

- `timeout_sec`: required. `0` returns at once with whatever is waiting (possibly nothing). Any other value waits until a report arrives or the time runs out; the maximum is `wait_timeout_max_secs` (daemon-configured, default 600, shared with [wait_for_status](#wait_for_status); see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)), and a larger value is `invalid_argument`.

**Output:**
```json
{
  "reports": [
    {
      "from_window_id": "w-task",
      "from_name": "parser",
      "outcome": "succeeded",
      "summary": "Parser rewritten; all parser tests pass.",
      "path": "reports/parser.md",
      "task": { "plan_id": "plan-7", "task_id": "build-parser" },
      "at_ms": 1700000000000
    }
  ]
}
```

When no report is returned -- a wait that timed out, or `timeout_sec` `0` with an empty inbox -- the reply also lists who you are waiting on:

```json
{
  "reports": [],
  "awaiting": [
    { "window_id": "w-task", "name": "parser", "status": "running", "mcp_connected": true },
    { "window_id": "w-shell", "name": "pwsh", "status": "idle", "mcp_connected": false }
  ]
}
```

`awaiting` holds every window that has not exited and whose report would be delivered to you right now -- decided by the same rule that addresses a report, so it never disagrees with where one would actually go. A window with `mcp_connected: false` has no MCP connection to report through, so no report can come from it until its CLI reaches yagura (see [list_windows](#list_windows) for what the value means). An empty list means no live window would report to you. The key is absent from a reply that returned reports; an absent `awaiting` is not an empty one. Each entry's `mcp_connected` means the same as in [list_windows](#list_windows).

`path` and `task` are `null` when absent. Reports already waiting are returned at once; a wait that times out returns an empty `reports` list, which is not an error. Every report is handed out exactly once: taking it removes it from the inbox. A report that arrives while a `wait_for_report` is waiting is handed straight to that wait (to the one that started first, when there are several) and never sits in the inbox. A wait cut short because its connection closed puts back anything it had already been handed, so no report is lost that way. A task report stays in the ledger after it is taken, so [get_plan](#get_plan) still shows it.

Only a window can wait for reports; any other connection gets `auth_failed`. A waiting `wait_for_report` counts toward the same per-connection limit on outstanding waits as `wait_for_status` (`resource_exhausted` beyond it), and is subject to the same blocking behavior from an MCP client's point of view: keep `timeout_sec` below your client's own request timeout. The inbox lives in the daemon's memory only: it is discarded when its window exits or is closed, and does not survive a daemon restart.

**The notice**: when a report lands in an inbox with no `wait_for_report` waiting, the daemon types one line into the parent window and submits it:

```
[yagura report] 1 pending report(s). Call wait_for_report to read them.
```

- It is typed only while the parent accepts input: the daemon waits until the parent's status is `waiting_input` or `idle` (held for its detection definition's `ready_hold_ms`, when set), and never types it while the status is `unknown`.
- It uses the same delivery machinery as a `send_handoff` envelope and waits its turn behind any other delivery into the same window. Once it holds that turn, and immediately before writing its first byte, it checks again that the parent still accepts input, is not `unknown`, and still has reports waiting; if not, nothing is written and the notice waits for the parent's next ready stretch (or is dropped, when the inbox has been emptied).
- It is typed once per stretch of reports: after one notice, no other is typed until `wait_for_report` has emptied the inbox, however many more reports arrive. A notice whose delivery does not go through is tried again while reports are still waiting: when the parent's status changes, or every 2 seconds while it does not. The reports stay readable with `wait_for_report` meanwhile.
- An empty inbox gets no notice, and neither does a report handed straight to a waiting `wait_for_report`.

The notice is not put through the approval gate: its text is the daemon's own fixed line, in which only the count varies and nothing a reporting window wrote ever appears, and it goes only to the window the reports are addressed to -- the window that opened the reporter, or the owner of the plan whose task it runs.

## Error Handling

`send_keys` returns `status_unknown` when its target is undetectable and `child_exited` when its target process has exited.

All tools return errors in the standard MCP format:

- **daemon_unavailable**: The yagura daemon is not running or connection failed
- **window_not_found**: The specified `window_id` does not exist. This also covers a `window_id` that was valid but has since been evicted from the registry (an unattached `exited` window can be evicted once the daemon needs the slot back), and a `window_id` from before the daemon's last restart (see [Window ID Validity](#window-id-validity))
- **invalid_argument**: Invalid input (e.g., empty command array, a NUL byte in an argv element, a character the command interpreter would reinterpret in a command line resolved through a `.cmd` / `.bat` shim, an unknown `profile` id, non-existent cwd, out-of-range `lines` / `timeout_sec`, `target_status: "unknown"`, a `send_handoff` with both or neither of `window_id` / `profile`, a `send_handoff` text field that is empty, over its byte limit, or carries a forbidden character, a `send_chat` / `broadcast_chat` / `read_chat` `room_id` that is neither `"main"` nor the caller's own workspace room (an approver's connection may name any room that exists), a `send_chat` / `broadcast_chat` `body` that has no visible character (empty, or only whitespace and zero-width joiners), over `CHAT_BODY_MAX_BYTES`, or carries a forbidden character, a `read_chat` `limit` outside `1..=CHAT_READ_LIMIT_MAX`, a `read_chat` from an unidentified connection with no `after_seq`, or a `status_event` with no `window_id`). A `model` or `effort` is refused the same way for any of three reasons: it was sent with a `command` (`open_window`) or with a `window_id` (`send_handoff`) instead of a `profile`; the named profile does not map that option at all; or the value is outside the launch option value rule (ASCII letters, digits and `. _ - : / + @`, not starting with `-`, at most 128 bytes). All three are refused before any process is started
- **spawn_failed**: Failed to start the child process
- **resource_exhausted**: Maximum window limit reached (`open_window`, `open_task_window`), too many concurrent `wait_for_status`, `wait_for_report` and `send_message` calls already outstanding on this connection (`this connection already holds the maximum number of pending waits`), too many deliveries already waiting for the target window's turn (`send_handoff`: `too many deliveries are already waiting on this window`; see [send_handoff](#send_handoff)), too many approval requests are already pending (daemon-wide or on this connection; see [Approval Flow](#approval-flow)), or too many `send_handoff` calls are still unanswered on this connection or across the daemon, whether each opened a window from a `profile` or addressed an existing `window_id` (see [send_handoff](#send_handoff)'s two-stage limit); the `message` names which of the two ran out: `too many handoffs are in flight (per-connection limit)` or `(daemon-wide limit)`
- **child_exited**: The target window has exited (`send_message`, `send_keys`, `interrupt_window`, a `send_handoff` addressed to an existing `window_id`, or a `wait_for_status` whose target status can no longer be reached), reported the same way to every caller regardless of origin. For a `send_handoff` this also covers a child that goes away while the envelope is being written; a child that exits after the write completes is not an error at all but a successful reply carrying `delivery: "unconfirmed"` (see [send_handoff](#send_handoff))
- **unsupported**: The requested feature is not supported. `send_chat` / `broadcast_chat` / `read_chat` return this when the daemon has no chat store to read or write -- either the configured playbook failed to load (chat is disabled along with everything else it would have relaxed), or the daemon could not open its chat database at start-up. Any tool also returns this when the running daemon's wire revision is too old to carry the request -- typically a daemon left running from before an upgrade; see [Starting the Daemon](#starting-the-daemon)
- **approval_required**: `interrupt_window` / `close_window` / `send_message` / `send_keys` / `send_handoff` / `send_chat` / `broadcast_chat` was refused before reaching a human decision at all -- either no approver is currently connected, or the configured playbook failed to load; for a `send_handoff` addressed to a `profile`, this can also happen before the window is even opened (see [send_handoff](#send_handoff)); a `send_chat` reaches this path whenever `send_chat` is not listed in `[approval].auto_approve` (see [send_chat](#send_chat)); see [Troubleshooting `approval_required`](#troubleshooting-approval_required)
- **status_unknown**: `interrupt_window` / `send_message` / `send_keys` / a `send_handoff` addressed to an existing `window_id` was refused because the target window's status is `unknown` (fail-safe; cannot be relaxed by a playbook or by being the target window's own connection, and is reported the same way to every caller — see [Troubleshooting `status_unknown` and `child_exited`](#troubleshooting-status_unknown-and-child_exited)). The `message` names why that status could not be detected, as a phrase in parentheses: `the window's status could not be detected (<reason>), so this operation is refused`, where `<reason>` is the English phrase for the same reason `unknown_reason` carries (see [Why a window is `unknown`](#why-a-window-is-unknown)); the error code itself is unchanged. A `send_handoff` also reports this when the target window's status has slipped back to `unknown` between an approval decision and delivery. Does not apply to `close_window`. `send_chat` / `broadcast_chat` never return this: an ineligible target is instead recorded as `"undelivered_unknown"` in [read_chat](#read_chat)'s `deliveries`, since a chat post may still reach other targets
- **approval_denied**: An approver decided against a queued `interrupt_window` / `close_window` / `send_message` / `send_keys` / `send_handoff` / `send_chat` / `broadcast_chat` request
- **approval_timeout**: A queued request was still undecided when the approval deadline elapsed (`[limits].approval_timeout_secs`, default 60s; see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)). A request to open a window (`open_window`, a `send_handoff` from a `profile`, `open_task_window`) whose requester's CLI dropped the call at the end of its turn (see the detection `[requests]` table) is withdrawn and answered this way too: no window was opened
- **approval_not_pending**: An `approval_decision` named a `request_id` that this connection is not (or is no longer) waiting to decide -- already settled, withdrawn, or never issued to this connection (see [Approval Flow](#approval-flow))
- **window_not_ready**: A `send_handoff`'s `profile` opened a window that did not reach a status that accepts input within `[limits].handoff_ready_timeout_secs` (see [send_handoff](#send_handoff)); the `message` names the last status observed, in its wire spelling followed by the reason in parentheses when that status is `unknown` (e.g. `unknown (no usable detection definition)`), any detection warning, and, when that window's definition asks for one, the hold it asked for in milliseconds -- plus, when that hold cannot be met inside that limit at all, that it cannot
- **not_written**: The daemon did not write a `send_message`'s text into the target window -- it looked at the window's composer, or at its screen, and never wrote the submit key for that text, so nothing was submitted. The reasons are the ones a `send_handoff` reports as `delivery: "not_written"` (see [send_handoff](#send_handoff)): another actor's text was already in the composer; the composer could not be observed; the body was written but the window did not take it -- it dropped the body (the definition's `probe`, typed behind it, showed alone), or the request was cancelled before the text showed, in which case the daemon took it back with the window's `clear` sequence; another actor wrote into the same window after the body and before the submit key, so the daemon wrote nothing more, took its text back in front of that write and checked the composer before answering; or the target's screen, at the instant the daemon was about to write, matched the detection definition's approval rule or one of its rules that declares the screen `unknown` -- an approval dialog is the common case -- so the daemon left it alone rather than let a submit key land on a prompt the receiving CLI has not settled yet. The first two reasons, a dropped body, and a take-back the daemon saw work, leave nothing behind in the window's composer; a take-back it could not see work and a write it had no way to take back leave something there, and so can the mixed-writes reason when its check could not confirm the text gone (see [send_handoff](#send_handoff)); the screen-not-accepting reason leaves nothing behind for a `send_message`, since it never strikes once a fresh body has been written -- the daemon waits for such a screen to accept input again instead -- and a `text` with no trailing terminator, or one that is only a bare terminator, is judged against this same screen rather than against the composer and is never partway written when this reason applies. Apart from its own take-back and probe, the daemon never erases what is left behind -- read the window with [`read_output`](#read_output) or [`get_window_status`](#get_window_status) before sending the same text again
- **no_composer**: `send_message` / `send_handoff`, or an `open_task_window` whose task carries an instruction, was refused because the target window's definition declares no composer -- a plain shell, or a command opened without an agent CLI's definition -- so the daemon could never type the text there as input; or because it declares one but names no `[input].probe`, without which the daemon cannot tell whether a line it writes has shown, so it writes none (`send_message` text with no terminator is still written there). Nothing of the text is typed into the window. Naming a `probe` in the definition lets such a window take these lines (see [Detection definitions](../crates/yagura-daemon/detection/README.md#composer)). To run a command, pass it as [`open_window`](#open_window)'s `command`; to press keys, use [`send_keys`](#send_keys).
- **rate_limited**: A `status_event` was discarded because the daemon-wide rate limit was exceeded (the connection stays open; retry after a short delay)
- **auth_failed**: A `status_event`'s `token` field was missing, malformed, or did not match the target window's token (see [StatusEvent Authentication](#statusevent-authentication)); also returned for an `identify` call whose token was malformed or did not match the target window's own token -- a credential mismatch of this kind counts toward a per-connection limit of 3 before the connection is closed, while a malformed token or an unknown `window_id` does not (see [Requester Identification](#requester-identification)); also returned for a `register_approver` whose peer is not running the executable named by `--approver-exe`, or when the daemon was started without `--approver-exe` at all -- this refusal counts toward no limit and leaves the connection open (see [Approver Registration](#approver-registration))
- **invalid_state**: A connection tried to claim an origin that conflicts with one it already established -- a second `identify` for a different window, or `register_approver` on a connection that already identified as a window (see [Requester Identification](#requester-identification)); also returned by `open_task_window` when the task it names already has a bound window that is still alive (see [open_task_window](#open_task_window))
- **plan_not_found**: `update_plan` / `get_plan` / `delete_plan` / `open_task_window` was called by a window outside the plan entirely -- neither its owner nor bound to any of its tasks -- or named a `plan_id` that does not exist at all; both cases answer identically, so a caller outside a plan cannot tell it apart from one that was never created
- **not_plan_owner**: `update_plan` / `delete_plan` / `open_task_window` was called by a window that is not the plan's owner, or by the owner window while it is no longer alive (closed, or its child process has exited)
- **plan_not_approved**: `open_task_window` was called by the plan's living owner while the plan's review is still pending; no window is opened. Once an approver approves the plan, a one-line notice starting with `[yagura plan]` is typed into the owner's window and the call succeeds (see [Confirming a plan](plans.md#confirming-a-plan))
- **profile_disabled**: `open_window` (with `profile`), `send_handoff` (with `profile`) and `open_task_window` refuse this way when the launch profile they would open a window from has been switched off in the user's settings (see [Disabling a profile](../profiles/README.md#disabling-a-profile)). This is not an argument error, so repeating the same request reworded will not help
- **internal**: A contract violation between the daemon and `yagura-mcp` (an undecodable or unexpected response), or any other failure that is not the caller's fault

## Delivery Guarantees

`yagura-mcp` sends every tool call as a single request over an IPC connection to the daemon and delivers it **at most once**: if the connection needs to be re-established (for example, after a transport-level failure), a request is only ever retried when the failure happened *before* that request's bytes had finished being sent. Once a request has been fully sent, any later failure (including one while waiting for the reply) is surfaced as an error to the caller and the connection is discarded — the request itself is never resent. This matters most for `interrupt_window`, `close_window`, `send_message` and `send_handoff`: a caller that sees an error for one of these calls can be sure the operation did not silently run twice, but should treat "did it happen at all" as unknown and re-check the window's state (e.g. with `get_window_status`) before retrying. For `send_handoff`, an error on this call also means any window it opened from a `profile` for this attempt has already been cleaned up by the daemon (see [send_handoff](#send_handoff)) -- there is nothing left to close by hand. A call the MCP client cancels (`notifications/cancelled`) is answered with an error at once, and its connection is dropped rather than reused; the daemon reads that as the requester leaving, so a gated call still pending approval is withdrawn and a `send_message` or `send_handoff` whose text has not shown in the receiver yet stops without pressing the submit key, taking the text back when the receiver's definition can (see [send_handoff](#send_handoff)'s `"not_written"`). As with any other error, re-check the window before sending the same text again.

## Approval Flow

`open_window`, `close_window`, `interrupt_window`, `send_message`, `send_keys`, `send_handoff`, `send_chat` and `broadcast_chat` are gated. Every connection to the daemon has an *origin*: `External` until it proves otherwise, `Window(id)` once it successfully identifies as a window, or `Approver` once it registers as one -- see [Requester Identification](#requester-identification) and [Approver Registration](#approver-registration). `yagura-mcp` identifies itself automatically, so a caller going through this MCP server is `Window(id)` whenever it runs inside a yagura window, and stays `External` otherwise; the GUI's own control connection registers as an approver.

The gate's decision, in order, for `close_window` / `interrupt_window` / `send_message` / `send_keys` / `send_handoff` (chat's own decision is a variant of the same shape, described after this list):

1. The target window's status: `interrupt_window`, `send_message`, `send_keys` and `send_handoff` (when addressed to an existing `window_id`) are refused outright as `status_unknown` when the target's status is `unknown`, regardless of origin or the playbook. For `send_handoff` this check runs again, against the window's then-current status, once an approval decision comes back -- see [send_handoff](#send_handoff). `close_window` is exempt.
2. The target window's detection definition declares a composer, and an `[input].probe` for text the daemon judges: `send_message` and `send_handoff` (when addressed to an existing `window_id`) are refused outright as `no_composer` when it declares no composer, and so are a `send_handoff` and a `send_message` whose text ends in a terminator when it names no probe, regardless of origin, the playbook, or whether the requester is the target window itself or a registered approver -- see [Detection definitions](../crates/yagura-daemon/detection/README.md#composer). `interrupt_window`, `send_keys` and `close_window` are exempt, since none of them types daemon-composed text into the window.
3. The requester is the target window itself -> allowed immediately, no approval needed (see [Requester Identification](#requester-identification) for who this covers). Since `send_handoff` addressed to a `profile` always opens a brand-new window, the requester can never be that window, so this step never applies to that form.
4. The requester is a registered approver -> allowed immediately.
5. The active playbook's `[approval].auto_approve` lists the tool -> allowed immediately.
6. Otherwise the request is queued for a human decision -- unless no approver is currently connected, or the configured playbook failed to load, in which case it is refused outright as `approval_required` instead of being queued (see [Troubleshooting `approval_required`](#troubleshooting-approval_required)). For a `send_handoff` addressed to a `profile`, this same decision is also made once, up front, before the window is even opened (see [send_handoff](#send_handoff)).

`open_window` has no target window either -- the window does not exist yet -- so it runs steps 4 to 6 only: a registered approver (the GUI) is never asked to approve opening a window, and every other origin, `External` included, is allowed when `[approval].auto_approve` lists `open_window` (the shipped default does) and is otherwise queued for a human decision. The one gate covers every way an agent can start a window: `open_window` itself (both the `command` and the `profile` form), a `send_handoff` addressed to a `profile`, and `open_task_window`. For a `send_handoff` the window's start is asked about first, before the process exists; the envelope's own approval (steps 1 to 6 above) follows once the new window is ready. For `open_task_window` the plan's own checks (the owner's window, an approved plan) come first and are unchanged. The playbook cannot make an agent's `command` safe: a window runs whatever it is given, outside the agent's own CLI permission prompts, which is why removing `open_window` from `auto_approve` makes every start wait for you.

`send_chat` and `broadcast_chat` have no single target window to run steps 1, 2 and 3 against -- they post to a room rather than type daemon-composed text into one window, so the composer gate is not their business either -- so they follow a variant: every origin runs steps 4 and 5 like any other tool, an unidentified (`External`) connection included, so a chat post skips the dialog only while the tool is listed in `[approval].auto_approve` -- which the shipped default does for `send_chat` and does not for `broadcast_chat`.

A queued request is sent to every registered approver as one entry of `ApprovalRequested`, carrying the tool, the requester (a window's name, or "external"), the target window's name and status **as observed at submit time** (omitted for `open_window`, `send_chat` and `broadcast_chat`, which have none), and -- for `open_window`, `send_message`, `send_handoff`, `send_chat` and `broadcast_chat` -- the full text body (the handoff envelope, for `send_handoff`; the chat envelope, for the two chat tools; for `open_window`, lines naming what would start: `command: <argv>` or `profile: <profile id>`, then `cwd: <directory>` and, for a window a handoff or a plan task opens, `for: handoff` or `for: plan task "<task id>"`; control characters in any of them are shown escaped). An `open_window` whose description would exceed the approval text limit is refused as `invalid_argument` rather than shown with its tail hidden. An approver settles it with a `request_id` and `approved: true`/`false`; every other approver is then told the dialog is no longer actionable. The caller's own reply is one of:

- **Approved**: the daemon re-checks the status gate against the window's *current* status -- not the one recorded when the request was submitted -- immediately before running the operation. A window that went `unknown` while the request sat waiting is refused as `status_unknown` even though it was approved: an approval decided against a stale status is not a licence to fire blind at whatever the window is doing now. `send_chat` / `broadcast_chat` have no single target to re-check this way -- an approved chat post is recorded and delivery begins for every currently eligible target, and one that has since become ineligible is recorded in [read_chat](#read_chat)'s `deliveries` as `"undelivered_unknown"` rather than failing the call.
- **Denied**: `approval_denied`.
- **The deadline elapses first** (`[limits].approval_timeout_secs`, default 60s -- see below): `approval_timeout`.
- **The requester's own connection disconnects** while the request is still pending: it is withdrawn and nobody is left to receive a reply. A request to open a window is also withdrawn when its requester's CLI drops the call at the end of a turn; that call is answered `approval_timeout` on its still-open connection.

Reconnecting as an approver (the GUI does this on every fresh control connection, including after a reconnect) returns every currently outstanding request in its response's `pending` field, ordered oldest first (submission order), so a GUI that reconnects mid-dialog gets its queue restored rather than starting empty. There is a cap on how many requests may be pending at once, daemon-wide and per connection; past it, submitting a new one is refused as `resource_exhausted` rather than displacing an older request. This is separate from `send_handoff`'s own two-stage limit on handoffs that have not been answered yet (see [send_handoff](#send_handoff)): a `send_handoff` still waiting on an approval decision counts toward both its own limit and this pending-approval cap at the same time.

**Concurrency from an MCP client's point of view**: `yagura-mcp` carries each tool call over a daemon connection of its own, at most four at once (see [`wait_for_status`](#wait_for_status)'s blocking semantics), so a gated call that is pending -- or a `send_message` / `send_handoff` waiting on its receiver -- does not hold up the same MCP client's other tool calls unless four calls are already outstanding; a call beyond that waits for one of them to end. A per-connection limit on the daemon side (pending approvals, outstanding handoffs, pending waits) applies to each of those connections separately.

**GUI presentation**: the approval dialog cannot be dismissed -- Escape and clicking outside it do nothing, only Approve or Deny close it, and the request stays pending regardless. If another dialog (add window, confirm, rename, restore) is already open, the approval dialog waits behind it and opens once that one closes, rather than stacking. A request that expires or is withdrawn without ever having been shown (queued behind another dialog, for instance) is instead surfaced afterward as a dismissible banner summarizing how many requests expired unseen.

**What an approval request does not carry**: the launch attributes of a `send_handoff` addressed to a `profile` -- its `model` and `effort` -- are not part of what an approver is shown, and are not part of the envelope either. They decide how the new window's CLI is started, not what is being asked of whoever ends up reading it; the tool, the requester, the target window and the full text body remain what the decision is made on.

## Playbooks: Approval and Limits

`open_window`, `close_window`, `interrupt_window`, `send_message`, `send_keys`, `send_handoff`, `send_chat` and `broadcast_chat` are the operations the approval gate covers. The daemon's default is **fail-closed: with no playbook resolved, none of the eight is ever auto-approved**, regardless of the target window's state -- though, as described in [Approval Flow](#approval-flow), a request from the target window itself or from a registered approver bypasses the playbook entirely and is never affected by this section.

A playbook is a TOML file that relaxes what still reaches a human. The daemon resolves the playbook path in two stages, in order:

1. `--playbook <path>` (command-line flag)
2. the `YAGURA_PLAYBOOK` environment variable

There is no executable-relative search: the daemon never infers trust from the shape of the filesystem (an install directory vs. a build directory, a co-located `Cargo.toml`, and so on). The caller is expected to name the file explicitly.

**The GUI seeds its playbook, then passes it explicitly.** At every start the GUI writes yagura's default playbook -- the one the repository ships at its root, which auto-approves only `open_window` and `send_chat` -- into the configuration root (Windows: `%LOCALAPPDATA%\io.github.satoh-y-0323.yagura\playbook.toml`; macOS: `~/Library/Application Support/yagura/playbook.toml`; Linux: `$XDG_CONFIG_HOME/yagura/playbook.toml`, falling back to `~/.config/yagura/playbook.toml`), **but only when no file exists there yet**. An existing `playbook.toml` is never read, rewritten, or overwritten by the GUI, so your edits survive restarts, upgrades, and reinstalls. The GUI then spawns the daemon with `--playbook <config-root>/playbook.toml` when that file exists; when it does not -- the configuration root could not be resolved, or the write failed -- it starts the daemon with no `--playbook` flag at all and the fail-closed default above applies. No manual copying is needed for the normal, GUI-launched case.

Two consequences of that shipped default are worth stating explicitly:

- **`auto_approve` still does not distinguish callers, only tools.** Listing a tool there means *any* non-self, non-approver requester -- a script driving another MCP client just as much as anyone else -- skips the approval dialog for that tool. The shipped default is `["send_chat", "open_window"]`, so out of the box every `close_window` / `interrupt_window` / `send_message` / `send_keys` / `send_handoff` / `broadcast_chat` request from another window or an external client reaches a human through the GUI's approval dialog, while `send_chat` does not (its reach is already bounded to chat participants, see [send_chat](#send_chat)) and neither does `open_window` (agents start windows as they always have; remove it to be asked each time); add entries to `[approval].auto_approve` yourself if you want a specific tool to bypass the dialog for every non-self, non-approver caller.
- **A daemon started outside the GUI is not covered.** The seeding happens in the GUI process, so a `yagura-daemon` you start yourself -- from a shell, a service, a test harness -- receives no playbook at all and denies every non-self, non-approver destructive call, even when the configuration root already holds one.

Running the daemon directly from the command line is otherwise unaffected by this fixed path -- pass `--playbook <path>` or set `YAGURA_PLAYBOOK` yourself, pointing at whichever file you want, including the seeded `<config-root>/playbook.toml` or the repository's own `playbook.toml`.

```toml
[approval]
auto_approve = ["send_chat", "open_window"]          # these two by default; list tool names to skip the dialog for them

[plans]                                              # optional; a missing table or key keeps the built-in default (true)
require_approval = true                              # whether a new plan waits for the operator's approve_plan before its tasks can open

[limits]                                             # optional; omitted fields (or the whole table) keep the built-in default
approval_timeout_secs = 60                           # deadline for a pending approval, in seconds (1..=600)
handoff_ready_timeout_secs = 60                      # deadline for a send_handoff's profile-opened window to
                                                      # become ready for input, in seconds (1..=600)
interrupt_bytes = [3]                                # byte sequence interrupt_window writes to the PTY
wait_timeout_max_secs = 600                          # upper bound accepted for wait_for_status's timeout_sec
chat_delivery_max = 32                               # chat deliveries (posts and intros together) in flight at once (1..=256)

[chat]                                                # optional; omitted fields (or the whole table) keep the built-in default
repetition_count = 4                                  # occurrences of the same room state that halt the room as a loop (1 or more)
repetition_window = 12                                # history window compared for repetition (1 or more)
repetition_distance = 6                               # maximum simhash Hamming distance still counted as the same state (0..=64)
monologue_max = 3                                     # consecutive posts from the same window before the room halts (1 or more)
turn_message_max = 30                                 # AI posts allowed since the last user post before the room halts (1 or more)
silence_timeout_secs = 300                            # seconds of no reply before the room needs the user (1..=86400)
history_max = 5000                                    # messages retained per room (1 or more)
intro = "..."                                         # one-time announcement posted to a participant window; empty disables it
exclude_detections = ["generic-shell"]                # detection definition names excluded from chat participation;
                                                       # a definition declaring no composer, or no probe, never participates either way
```

- `[approval].auto_approve` lists MCP tool names (the external, stable tool names -- `open_window`, `close_window`, `interrupt_window`, `send_message`, `send_keys`, `send_handoff`, `send_chat` and `broadcast_chat`) that may run without a human decision. There is no wildcard; each tool must be listed explicitly. An unrecognized tool name in the list is not an error -- it is logged as a warning and ignored, so a playbook stays forward-compatible with future tool additions.
- `[plans]` is optional. `require_approval` defaults to `true` -- a missing `[plans]` table, or the table present with the key omitted, still means every new plan waits for the operator's `approve_plan`. Switching it from the GUI's Settings > Approvals tab (see [Changing approvals from the GUI](#changing-approvals-from-the-gui) below) takes effect at once, but only for a plan created after the switch -- a plan already in the ledger keeps the review state it started with. Editing the key directly in the file instead still needs a daemon restart to take effect. See [Confirming a plan](plans.md#confirming-a-plan) for what the gate refuses and how it is settled.
- `[limits]` is optional. Only the fields actually present override the daemon's built-in defaults; anything omitted (including the whole `[limits]` table) keeps the default.
- `[limits].approval_timeout_secs`, when present, must be within `1..=600`; a value outside that range fails the **whole playbook** to load, not just this key -- the same as any other schema violation. The daemon's own built-in default is 60s, so omitting the key has the same effect as writing `60`.
- `[limits].handoff_ready_timeout_secs`, when present, is validated the same way and against the same range as `approval_timeout_secs`: a value outside `1..=600` fails the whole playbook to load, and the rejection message names `handoff_ready_timeout_secs` specifically so it is not mistaken for the other key. The daemon's own built-in default is also 60s.
- `[limits].chat_delivery_max`, when present, must be within `1..=256`; a value outside that range fails the whole playbook to load, the same discipline as the two deadlines above. The daemon's own built-in default is 32. A delivery counts toward it only while it holds its target window's turn, not while it waits for one.
- `[chat]` relaxes the chat rooms' own limits and the intro text; every field is optional and defaults to the built-in value shown in the example above (`repetition_count` 4, `repetition_window` 12, `repetition_distance` 6, `monologue_max` 3, `turn_message_max` 30, `silence_timeout_secs` 300, `history_max` 5000, a non-empty `intro`, and `exclude_detections` of `["generic-shell"]`). Every numeric field is range-checked the same way the `[limits]` deadlines are -- a value outside the built-in range for any single field fails the **whole playbook** to load. **When the playbook fails to load for any reason (an out-of-range `[chat]` or `[limits]` value, an unknown key, or any other schema violation), chat itself is disabled for the whole daemon**: `send_chat`, `broadcast_chat` and `read_chat` all return `unsupported` until the daemon is restarted with a playbook that loads -- this is stricter than the other six gated tools, which merely deny individual requests when the playbook is broken.
- **Unknown keys anywhere in the file (an unrecognized section or field, including typos) make the whole file fail to load.** This is deliberate: a mistyped setting must be a loud failure, never a silently-ignored one.
- An empty file, or a file with `[approval]` omitted entirely, is valid TOML and loads successfully -- approval then auto-approves nothing, the same as no playbook at all.
- **The daemon does not exit when a playbook fails to load or the configured path does not exist.** It logs a warning and continues running with every non-self, non-approver destructive operation denied (fail-closed degrades gracefully; a broken playbook must never take down the windows it would have governed).
- `[approval].auto_approve` and `[plans].require_approval` take effect the moment they are switched from the GUI's Settings > Approvals tab (see [Changing approvals from the GUI](#changing-approvals-from-the-gui) below). Every other setting in this file -- and a change to either of those two made by editing the file directly, rather than through that tab -- is still read exactly once, at daemon startup: there is no hot reload for them, and changing them requires restarting the daemon.
- Operational parameters other than `approval_timeout_secs`, `handoff_ready_timeout_secs`, `interrupt_bytes`, `wait_timeout_max_secs`, `chat_delivery_max` and `[chat]` (e.g. the close grace period, rate-limit burst/refill, `read_output` bounds) are built-in defaults and cannot be overridden by a playbook.

### Changing approvals from the GUI

The GUI's Settings panel (the gear button in the navigation rail) has an **Approvals** tab: a switch for each of the eight gated tools' `auto_approve` entry, and one more for `[plans].require_approval`. These two settings are the only ones this tab -- or anything else -- can change while the daemon keeps running; every other playbook setting is still read only at startup, as above. There is no MCP tool for this: only a registered approver, from the GUI, can switch either setting.

- **It takes effect immediately, for what happens next.** A tool switched to auto-approve stops raising the approval dialog for the very next request that needs it; switching `require_approval` off means the very next `create_plan` is not gated. Neither is retroactive: a request already waiting for a human decision when a tool is switched on stays waiting, and a plan already in the ledger keeps the review state it had when it was created (see [Confirming a plan](plans.md#confirming-a-plan)).
- **The file and the daemon's running decision are judged separately.** Pressing a switch rewrites the file only when the file's own current value disagrees with it, and changes (and logs) the daemon's running decision only when *that* disagrees with it -- the two checks do not depend on each other. A hand edit of the file is never picked up by a switch itself, and is only read the next time the daemon starts, so the file and the daemon's decision can drift apart in the meantime (and the tab can disagree with the file on disk) until either a switch or a restart brings them back together. Because the two checks are independent, a switch that only needs to catch the daemon's decision up to a file already hand-edited to match it changes the daemon's decision (and logs it) without writing the file again; one that only needs to catch the file up to a daemon decision a hand edit had not touched rewrites the file without the daemon's decision changing (and logs nothing, since nothing about the running decision changed).
- **The rewrite touches only the one thing requested.** The rest of the file -- comments, every other table, and its line endings -- is left exactly as it was. On Unix, writing the file also narrows the directory it is placed in, and the file itself, to owner-only access, the same way the GUI's initial seeding of the file does (see [Checking the configuration root](#checking-the-configuration-root)); on Windows this is a no-op, since NTFS inheritance from `%LOCALAPPDATA%` already scopes the directory to its owner.
- **Deleting the file** does not change the running daemon's own decision. It only means the next time the GUI starts, it seeds a fresh copy of the shipped default there again (see above); the running daemon still needs a restart to pick that copy up.
- **Every successful switch is logged as one line** naming the tool (or `plans`) and the new value it was set to. When the playbook was never configured, or it failed to load at daemon startup, the whole tab is read-only with a one-line notice instead of a switch to press. When the playbook loaded at startup but a switch cannot be carried out anyway -- the file has since become unreadable, or writing it back failed -- that one switch reverts and a one-line failure message appears at the top of the tab (the rest of the tab stays usable); the daemon logs a warning, and both the file and its own decision are left untouched.

### Troubleshooting `approval_required`

`approval_required` means the request was refused **before it could even reach a human**. The `message` field tells you which of two states produced it:

1. **`"no approver is connected, so <tool> is denied"`**: nobody is currently registered as an approver to decide the request -- usually because the GUI is not running, or its control connection has not finished registering (see [Approver Registration](#approver-registration)). This is the ordinary case, and it is not specific to a broken or missing playbook: as long as an approver *is* connected, a missing playbook, an empty one, or one whose `auto_approve` does not list this tool all land in the approval queue instead of being denied outright -- this message means literally nobody was there to decide it.
2. **`"the configured playbook failed to load, so <tool> is denied"`**: `--playbook` or `YAGURA_PLAYBOOK` resolved a path, but reading or parsing it failed. Check the daemon log (see [Debugging Connection Issues](#debugging-connection-issues)) for a warning line -- grep for `failed to load the playbook`, which names the exact error (a missing file reads as an I/O error, an unknown key or wrong-typed value as a TOML parse error).

For a `send_handoff` addressed to a `profile`, both messages can also appear for this same reason **before the window is ever opened** -- this pre-launch check (see [send_handoff](#send_handoff)) asks the same question the gate would ask after opening the window, just earlier, so it never spawns a process it already knows it cannot deliver into. This is separate from an unknown `profile` id, which is always `invalid_argument` regardless of whether an approver is connected or the playbook loaded.

### Troubleshooting `status_unknown` and `child_exited`

Both are reported the same way to every caller, regardless of origin or the playbook: there is no partial disclosure that hides a window's state from an external caller, since `get_window_status` already lets anyone read it.

- **`status_unknown`** applies to `interrupt_window`, `send_message` and `send_handoff` (not `close_window`, which stays closable even when undetectable). For `send_handoff` this covers an existing `window_id` target directly, as well as the re-check made once an approval decision comes back for either a `window_id` or a freshly opened `profile` target -- see [send_handoff](#send_handoff). A window whose detection definition never matches its own output stays `unknown` forever, so `send_message` can never reach it -- not even a call from inside that same window. The way out is to assign a detection definition that actually matches the window's output (`open_window`'s `detection` argument; see [Detection definitions](../crates/yagura-daemon/detection/README.md)). A status reported with `yagura-daemon --emit-status` does not make such a window accept input: a report only marks a turn as open or closed, and a closed turn leaves the status to the screen (see [Structured sources](../crates/yagura-daemon/detection/README.md#structured-sources---emit-status)).
- **`child_exited`** is checked before the approval gate for `interrupt_window` and for a `send_handoff` addressed to an existing `window_id`, so a target that has already exited never generates an approval request at all.

#### Why a window is `unknown`

Every `unknown` the daemon reports carries a reason. It reaches you as `unknown_reason` in [list_windows](#list_windows), [get_window_status](#get_window_status) and an `"undelivered_unknown"` delivery record in [read_chat](#read_chat), as the phrase in a `status_unknown` refusal's `message`, and beside the status in the GUI. The six reasons and what each asks of you:

| `unknown_reason` | What it means | What to do |
| --- | --- | --- |
| `no_definition` | No usable detection definition is assigned, so the status can never be detected. | Assign one with `open_window`'s `detection` argument or fix the profile. `detection_warning` says why this window has none: a name no layer carries, a definition file that could not be read or parsed (the failure is named), a detection directory that could not be listed at all, or no definition layer configured at all. |
| `no_rule_matched` | The screen matches no rule of the detection definition. | Look at the window; messages stay refused until a rule matches or the definition settles this window on time alone. Read the window with [read_output](#read_output) and compare it with the definition's patterns; [Detection definitions](../crates/yagura-daemon/detection/README.md#why-a-window-is-unknown) names the daemon log line that carries the exact lines the rules were matched against. |
| `screen_lost` | The screen model was lost and has not been trusted again. | Look at the window; the status recovers once the window settles on a state that accepts input. A loss is a failure inside the daemon's terminal model, which the daemon log records as `the virtual terminal screen model panicked and was rebuilt blank`. |
| `detector_stopped` | Detection stopped reporting for this window. | Close the window and open it again. |
| `not_yet_judged` | The window just opened and nothing has been judged yet. | Wait for output. |
| `declared_unknown` | A detection rule reported the status as unknown. | Look at the window: something reported the state as undetermined on purpose, so the screen is where the answer is. |

A window that has lost its screen model is reported as `unknown` for as long as that lasts, rather than as a status derived from how recently output arrived, so a `send_message`, an `interrupt_window`, a handoff or a chat delivery aimed at a window just after a rebuild can be refused. `close_window` still works throughout.

#### Where the daemon log is

Windows:

```
%LOCALAPPDATA%\io.github.satoh-y-0323.yagura\logs\daemon.log
```

macOS:

```
~/Library/Application Support/yagura/logs/daemon.log
```

Linux:

```
$XDG_STATE_HOME/yagura/logs/daemon.log
```

(`~/.local/state/yagura/logs/daemon.log` when `XDG_STATE_HOME` is not set.)

This is the *state* root, not the configuration root that holds `playbook.toml`; the two resolve to the same directory on Windows and macOS (both platforms use a single base directory for state and configuration), but differ on Linux (`XDG_STATE_HOME` vs. `XDG_CONFIG_HOME`). If the daemon could not open that file it falls back to logging on stderr and says so in its first line.

Each daemon start writes exactly one of these two lines:

- `resolved playbook: <path>` — a path was supplied and the daemon tried to load it. If it failed, every non-self, non-approver destructive call is refused as `approval_required` (case 2 above); if it loaded, whether a call is still refused as `approval_required` (case 1) or is queued for a human decision instead depends on whether an approver is currently connected.
- `no playbook resolved` — no path was supplied at all. Nothing is auto-approved by a playbook, but a request from the target window itself or a registered approver still runs, and any other request is queued for a human decision as long as one is connected -- refused as `approval_required` (case 1) only when none is.

#### Checking the configuration root

When a GUI-launched daemon logs `no playbook resolved`, the seeding described in [Playbooks: Approval and Limits](#playbooks-approval-and-limits) did not leave a file behind. Check, in the configuration root (`%LOCALAPPDATA%\io.github.satoh-y-0323.yagura` on Windows, `~/Library/Application Support/yagura` on macOS, `$XDG_CONFIG_HOME/yagura` — or `~/.config/yagura` — on Linux):

1. **Does `playbook.toml` exist there?** If the directory itself is missing, the GUI could not resolve or create the configuration root; on Windows that means `LOCALAPPDATA` was not set for the GUI's process, on macOS that `HOME` was not set, on Linux that neither `XDG_CONFIG_HOME` nor `HOME` was.
2. **Can your user read and write it?** yagura only ever seeds and reads the file as the current user, and narrows the directory and file it creates to owner-only on Unix. A `playbook.toml` owned by another user, or a directory you cannot write, blocks the seeding and is left untouched; fix the ownership, or place your own copy of the repository's `playbook.toml` there by hand.
3. **Is a stale daemon still running?** The playbook is read once, at daemon startup. A daemon that was already running when the file appeared keeps its old decision until it is restarted.

## Requester Identification

A connection's origin starts as `External` and can be upgraded exactly once, in one of two mutually exclusive directions:

- **`identify { window_id, token }`** claims the connection as that window, proven with the window's own per-boot status token (see [StatusEvent Authentication](#statusevent-authentication) for how the token reaches a process). `yagura-mcp` sends this automatically, once, on every fresh connection it opens, using the `YAGURA_WINDOW_ID` / `YAGURA_STATUS_TOKEN` environment variables it inherits from its own process's environment -- populated only when `yagura-mcp` is itself running inside (or spawned from within) a window the daemon opened. A bridge started outside any such window -- a person's own MCP client, not launched by an agent CLI running in a yagura window -- never sees these variables and stays `External` for its whole lifetime. The same happens inside a window when the MCP client does not pass its own environment on to the server it spawns: Codex CLI assembles an stdio server's environment from an allow-list, so both variables have to be forwarded explicitly -- by the `env_vars` override a bundled profile's `[mcp]` table injects (see [Windows yagura Opens](#windows-yagura-opens)), or by its own configuration (see [Configuring Codex CLI](#configuring-codex-cli)). Claude Code passes its environment through unchanged and needs no such setting. A WSL window's variables cross into the distribution only because the daemon names them in `WSLENV` (see [docs/wsl.md](wsl.md#environment-variables)).
- **`register_approver`** claims the connection as an approver (see [Approver Registration](#approver-registration)).

**Who this identifies**: the token lives in the environment of the window's *whole process tree*, not just its immediate child, so the self-window exemption (see [Approval Flow](#approval-flow)) is granted to any process running inside that tree that can read its own environment and open a connection to the daemon. This does not exceed what such a process could already do directly -- write to its own PTY, or terminate anything sharing its Job Object / process group -- so it is accepted as within the daemon's existing trust boundary.

**Idempotence and conflicts**: re-asserting the same claim (the same `window_id`, or registering as approver again) succeeds and leaves the origin unchanged. Claiming a *different* origin than the one already established -- a second `identify` for another window, or `register_approver` on a connection that already identified as a window -- is refused as `invalid_state`, and the origin is left untouched; `External` is the only origin from which either claim may still be attempted. `identify` failures that are credential mismatches (a well-formed token that does not match the target window's) count toward a per-connection limit of 3 before the connection is closed; a malformed token and an unknown `window_id` do not count.

**Limits under `YAGURA_STATUS_TOKEN_OVERRIDE`**: with the override active (see [StatusEvent Authentication](#statusevent-authentication)), every window shares the same token, so `identify` no longer distinguishes one window from another -- any connection that knows the override value can identify as *any* `window_id`. This is a test/development mechanism only; do not run it in production, where window identity would then carry no real guarantee.

When `yagura-mcp`'s own `identify` is refused, it does not retry: the connection stays `External` for that connection's lifetime (a fresh connection, for example after a daemon restart, attempts `identify` again from scratch). Every gated tool call's JSON response carries a `"requester"` field (`"window"` or `"external"`) reflecting this, plus, only when a claim was refused, an `"identity_warning"` field explaining why.

## Approver Registration

Registering as an approver grants two things at once: every current and future pending-approval notification, and -- like a window acting on itself -- every destructive operation aimed at *any* window without going through the gate at all.

**Who may register**: only a peer running the executable named by the daemon's `--approver-exe <path>` startup argument. This requirement sits on top of the same-OS-user check every IPC connection already requires, it does not replace it. The GUI passes its own executable path when it starts the daemon, so in a normal setup the GUI's control connection is the only one that can register. The daemon reads the peer's executable path from the OS once, when the connection is established, and compares it with the configured path as a plain equality of two already-canonicalized paths (the argument is resolved at daemon startup, the peer's path when the connection is accepted). An executable replaced after a connection was accepted is not picked up until the next connection.

**Without `--approver-exe` the daemon is fail-closed**: no connection can become an approver, so every destructive operation the playbook does not auto-approve ends as `approval_required`. The daemon writes one warning to its log at startup -- `approver registration disabled: --approver-exe was not given`, or `approver registration disabled: --approver-exe could not be resolved: {err}` when the given path cannot be resolved -- so check the daemon log first if approvals never reach a human.

**A refused registration does not close the connection**: `register_approver` from any other peer is answered `auth_failed`, the connection keeps whatever origin it already had, and the refusal is logged once per connection. Unlike an `identify` credential mismatch it does not count toward the per-connection limit of 3 that closes a connection, because the GUI's control connection retries registration on a timer and closing it would turn a version mismatch into a reconnect loop.

**A daemon the GUI did not start can refuse its registration**: the GUI prefers an already-running daemon over starting its own, and a daemon outlives the GUI that started it, so the GUI may be talking to a daemon that was started without `--approver-exe` -- for example, one you started yourself from a shell -- which therefore refuses the registration. Run `yagura-daemon --shutdown` once, then start the GUI. When registration is refused the GUI says so with a banner: "Approvals are unavailable: restart the daemon (yagura-daemon --shutdown). Restarting the daemon closes every window that is currently open."

Two consequences follow directly from what registration grants:

- (a) once your own connection registers as an approver, your own subsequent destructive operations on *every* window skip the approval gate, not just a window you already own.
- (b) a registered approver receives the full content of every pending request, including a `send_message` body's full text or a `send_handoff` envelope's full text, the requesting window's name, and the target window's status at submit time -- there is no per-request scoping or redaction.

**Residual risk**: verifying the peer's executable is not a boundary against an attacker who already runs as the same OS user. Such an attacker can replace the executable the daemon is configured to trust, inject into the running GUI process, drive the GUI's Approve button with synthetic input from the same desktop, or rewrite the playbook so the operation is auto-approved. What the check does remove is the one-frame bypass: disabling the gate no longer costs a single `register_approver`, it costs a visible GUI launch or a modification of the executable on disk.

**Residual risk (pid reuse)**: the peer's executable is resolved through the connecting process's pid (Windows: `GetNamedPipeClientProcessId`, then `OpenProcess`; Linux: `SO_PEERCRED`; macOS: `LOCAL_PEERPID`). The pid is fixed when the peer connects, while the daemon samples it later, inside the task it spawns for the connection it has accepted: **the window of concern therefore runs from the connecting process's exit to the daemon's sampling, and it includes the time the connection spends waiting to be accepted and, once accepted, waiting for its task to be scheduled** (no timing figure is claimed here -- opening many connections at once makes the daemon accept each one later). If, within that window, another process is handed the same pid and that process is running the executable named by `--approver-exe`, the daemon would sample the wrong executable and let the registration through. The connection itself has to stay open the whole time, which means another process must have inherited (or duplicated) its file descriptor / handle and be holding it: a connection normally closes when the process that opened it exits, so the only shape that works is "pass the connection to a child, then exit the parent". **This stays inside the same-OS-user residual risk above for two reasons**: connecting at all is restricted to the same OS user, and the pid must be reused by a process that is itself running the approver executable (no privilege condition is placed on pid recycling itself). Resolving this at the root needs a process identifier that does not go through a pid; what the supported platforms offer for that differs between them, so it is not addressed.

**The GUI's own control connection registers automatically**, and re-registers on every fresh connection (including a reconnect), but **never sends a destructive operation (`close_window` / `interrupt_window`) before that registration has completed**. While registration is still outstanding, the corresponding action fails internally rather than sending the request unguarded, and is surfaced to the WebView through the same operation-error banner as any other failed close/interrupt.

## Interrupt Effectiveness

`interrupt_window` writes the configured byte sequence (`0x03`/Ctrl-C by default) directly to the window's PTY input. Whether this actually interrupts the target CLI depends on that CLI's own console input handling — under ConPTY on Windows, `0x03` is translated into the console interrupt event before the child ever sees the raw byte, and on macOS and Linux the pseudo-terminal's line discipline turns it into `SIGINT` for the foreground process group unless the program has switched that off (as a program reading raw input does). That is how most interactive CLIs behave when a user presses Ctrl-C at a real terminal, but a CLI that does not react to the event or signal, or reads the raw byte and ignores it, will not be interrupted. If interrupting a particular CLI does not work, override the byte sequence for it via a playbook's `[limits].interrupt_bytes` (for example, an escape key sequence instead of Ctrl-C).

## StatusEvent Authentication

`status_event` (the message the daemon's `--emit-status` helper, and by extension a child CLI's own hooks, use to report status) requires a `token` field. The daemon rejects any event whose token is missing, does not match, or is the wrong length — all as `auth_failed`.

**Mechanism**: tokens are **per window**. When the daemon opens a window it generates a fresh 128-bit token (from the OS RNG) for that window alone, never persists or logs it, and injects it into that child's environment as `YAGURA_STATUS_TOKEN`, together with that window's own id as `YAGURA_WINDOW_ID`. A child CLI's hooks (or any code running inside that child's process tree) inherit both environment variables and pass them along when invoking `yagura-daemon --emit-status`, which reads them and forwards them as the `status_event`'s `token` and `window_id`. `YAGURA_WINDOW_ID` is only a matching key that tells the daemon which window's token to compare against; knowing it grants no authentication on its own, since a process that only knows another window's id still cannot obtain that window's token. The daemon resolves the event's target window first and then compares the token against *that window's* token, so:

- A process with no route to any such environment variable cannot authenticate at all.
- A process that holds one window's token can only report status **for that window**. Reading the environment of window A does not let anything speak for window B — an event aimed at B with A's token is rejected as `auth_failed` and leaves B's status untouched, regardless of which `window_id` accompanies it.

**`--emit-status` window attribution**: the helper reads `YAGURA_WINDOW_ID` from its environment and sends it as the event's `window_id`, so a hook running anywhere in a spawned window's process tree is attributed to that window. When `YAGURA_WINDOW_ID` or `YAGURA_STATUS_TOKEN` is unset -- outside any window the daemon spawned -- the helper sends nothing and exits successfully, so a CLI hook that also runs in sessions yagura did not open stays quiet there. A `status_event` sent some other way without `window_id` is refused as `invalid_argument`: no other window stands in for the one it does not name.

Because the target window is resolved before the token comparison, an unauthenticated caller can tell a live `window_id` from an unknown one (`window_not_found` versus `auth_failed`). No secrecy is claimed for `window_id`s, and none is relied on here: as stated above, a `window_id` is only a matching key, and authentication is carried solely by that window's token. The boot nonce a `window_id` carries exists to keep identifiers from different daemon instances from colliding — it is not a secret and is not intended to make ids unguessable (see [Window ID Validity](#window-id-validity)). So learning that an id is live tells a caller nothing it can act on: reporting status for that window still requires that window's token.

**Limits**: this raises the bar within the same OS user, but **does not change the underlying trust boundary, which remains the OS user account**. Any other process running as the same OS user can read the environment variables of processes it can inspect (including, on Windows, another process's environment if permitted by the OS), so a determined same-user attacker who can read the *target* window's environment is not stopped by this token — it stops *accidental or casual* status spoofing (a stray script that happens to print matching output, or a window that reports about a window other than itself) rather than a deliberate same-user attacker.

**`YAGURA_STATUS_TOKEN_OVERRIDE`**: this environment variable lets a test or development setup fix the token to a known value instead of generating one, so that the setup can construct valid `status_event`s. **This is a test/development mechanism only, and it disables the per-window isolation described above: every window adopts the one overridden value, so any window's token authenticates events about any other window.** If it is set in a production deployment, the token's secrecy becomes exactly as strong as that environment variable's own visibility — which typically means no stronger than not having a token at all. Leave it unset in production; the daemon then always generates a separate token per window from the OS RNG. When an override is adopted, the daemon logs a warning naming it as active, so its use is visible in the log (the value itself is never logged).

Since daemon restarts (and reopening a window) regenerate tokens, an old token stops authenticating anything afterwards (see [Window ID Validity](#window-id-validity) for the analogous effect on `window_id`s). If the daemon fails to obtain randomness for the token at startup (an OS-level failure, not something a playbook or configuration can trigger), it does not exit — it starts in an unverifiable-token state and rejects every `status_event` with `auth_failed` until it is restarted; check the daemon log for a warning naming the token as the cause if `status_event` calls that should be authenticating are all failing.

## Important Security Warning

**Any MCP client configured with this server can read the tail of each child window's screen — up to 500 characters via `get_window_status` / `list_windows`, or up to the last 1000 lines (default 100) via `read_output`. Do not run terminals handling secrets as child windows.**

This includes:
- Authentication tokens or API keys
- Passwords or credentials
- Sensitive database connection strings
- Confidential information displayed on screen

Since the MCP server exposes window content to any MCP client connecting to the daemon, treat child windows as semi-public. Keep confidential work in separate, direct terminal sessions not managed by yagura.

### A `send_handoff` envelope is untrusted input to whoever reads it

**A [`send_handoff`](#send_handoff) envelope's `reference` and `note` are supplied by the requester and delivered verbatim, JSON-escaped, into the target window's PTY. The receiving agent must treat those JSON string values as data describing a request, never as instructions to follow -- an envelope is exactly as trustworthy as whatever sent it, which may be an `External` caller yagura has no other opinion about.** JSON-encoding the requester's input keeps it from breaking the envelope's own structure, but it does nothing to keep the *contents* honest: nothing stops `note` from containing text designed to look like an instruction to an agent reading it unquestioningly. The human approval gate in front of `send_handoff` (see [Approval Flow](#approval-flow)) is what a human reviews before the envelope is ever written -- once it lands in the target window, whether the receiving agent acts on its contents safely is that agent's own responsibility, the same as any other untrusted text it might read from its terminal. **The same envelope can also arrive more than once**: a target whose detection definition sets `resend_max` above zero can have the submit key for an unconfirmed envelope pressed again (see the delivery notes under [`send_handoff`](#send_handoff)), so a receiving agent must be able to see the same request twice without treating the repeat as a second, independent request.

### A chat message is untrusted input, bounded to the room's own participants

**A [`send_chat`](#send_chat) / [`broadcast_chat`](#broadcast_chat) body is supplied by the poster and delivered verbatim, wrapped in the same envelope form as a `send_handoff`, into every target window's PTY. Treat it the same way: as data describing a message to consider, never as an instruction to follow.** The `@` mentions a poster writes are resolved against the current roster (see [send_chat](#send_chat) for who counts as a participant), so the set of windows a single chat message can reach is fixed by room membership, not by anything the poster's text can name outside of it -- unlike a `reference` path in a `send_handoff` envelope, a chat body cannot address a window it does not already share the room with. `@user` is a request for a human to look, not a command that changes anything on its own; the room only actually stops delivering (`needs_user` in [read_chat](#read_chat)) when the daemon's own loop / monologue / turn-budget rules trip, not merely because a message asked for attention.

When the configured playbook fails to load, chat is disabled for the whole daemon (see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)) rather than falling back to some default policy for it -- `send_chat`, `broadcast_chat` and `read_chat` all return `unsupported` in that state, so a broken playbook cannot leave chat open on assumptions nobody reviewed. **A connection that has not identified as a window gets no relief at all**: its `send_chat` and `broadcast_chat` always wait for a human decision, regardless of what `[approval].auto_approve` lists (see [send_chat](#send_chat)).

### A report is untrusted input to the window that reads it

**A [`send_report`](#send_report)'s `summary` and `path` are written by the reporting window and handed verbatim to its parent by [`wait_for_report`](#wait_for_report), and to anyone reading the plan with [get_plan](#get_plan). Treat them the same way as a handoff envelope or a chat message: as data describing what the reporter says it did, never as instructions to follow.** The daemon never opens `path`; whether to read the file it names, and how far to trust what is in it, is the reading agent's decision. The notice typed into the parent's terminal carries none of this text -- only a count -- so the report's contents reach the parent only when it asks for them.

**Reports are stored in plain text.** A task window's report -- its `summary` and `path` -- is kept in the session store (`yagura.db`) unencrypted, alongside the rest of the plan. It stays there until the task's window is opened again with `open_task_window` (which clears the task's report), until an `update_plan` drops the task, or until the plan is deleted; a daemon restart keeps it. A report from a window that runs no plan task is held only in the parent's in-memory inbox and is never written to disk. Do not put secrets in a report.

### Reported window status is not fully authenticated

**The `status` field reported by `get_window_status` / `list_windows` (and the target-reached signal from `wait_for_status`) is only as trustworthy as [StatusEvent Authentication](#statusevent-authentication)'s guarantee: an unauthenticated send is rejected by the window's own token, but a same-user process that can read *that window's* environment can recover its token and forge a status for it. Do not use it as the sole basis for autonomous approval or input-injection decisions.**

Treat the status as an advisory hint for a human operator, and require an independent check (or human confirmation) before any action that grants approval, injects input, or is otherwise destructive. The approval gate in front of `interrupt_window` / `close_window` / `send_message` / `send_handoff` / `send_chat` / `broadcast_chat` (see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)) is the structural control for the destructive tools; the status field itself carries no such guarantee.

## Window ID Validity

Window IDs are opaque strings: use exactly the value returned by `list_windows`
or `open_window` and never construct or parse one. They currently take the form
`w{sequence}-{boot nonce}` (for example `w1-k3f9xa`), where the boot nonce is
generated once per daemon process, so IDs are unique per daemon lifetime.

When the daemon restarts:
- All existing window IDs become invalid. The sequence restarts, but the new
  boot nonce means a stale ID from the previous daemon is rejected with
  `window_not_found` rather than silently matching a different window
- New windows are assigned fresh IDs
- Callers should call `list_windows` after reconnection to discover active windows

A `window_id` can also stop resolving without a daemon restart: an unattached, already-`exited` window may be evicted from the registry once the daemon needs the slot back (to stay within its total-entry limit). Any tool call against an evicted `window_id`, including one already blocked inside `wait_for_status`, resolves as `window_not_found`.

## Debugging Connection Issues

If the MCP server fails to connect:

1. Verify the daemon is running:
   ```bash
   # Windows: check if the yagura-daemon.exe process exists
   tasklist | findstr yagura-daemon
   # macOS / Linux
   pgrep -l yagura-daemon
   ```

2. Verify the MCP binary path is correct in your client settings

3. Ensure both the daemon and client run under the same OS user account

4. Check client logs for connection errors

5. If a destructive tool call unexpectedly returns `approval_required`, see [Troubleshooting `approval_required`](#troubleshooting-approval_required)

6. If `YAGURA_INSTANCE` is set for either the daemon or `yagura-mcp` but not the other, they resolve
   to different endpoints and never see each other. Confirm both processes were started with the
   same `YAGURA_INSTANCE` value (or both left it unset) -- see [Connecting to a Second
   Instance](#connecting-to-a-second-instance)

7. If a window yagura opened from a profile stays at `mcp_connected: false`, check in order: that
   the profile has an `[mcp]` table (the bundled ones do; a profile of your own needs its own -- see
   [profiles/README.md](../profiles/README.md#where-profiles-live)); that the daemon log does not say
   `no usable mcp bridge next to the daemon`, or that it dropped the injection for that window (see
   [profiles/README.md](../profiles/README.md#mcp-injection)); and that `yagura-mcp` itself did not
   write `yagura-mcp: could not identify with the daemon at startup: <reason>` to its stderr
