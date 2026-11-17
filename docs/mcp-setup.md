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
for Codex CLI, GitHub Copilot CLI and Grok Build), so a development instance's registration never overwrites, and
is never overwritten by, the default instance's `yagura`. The default instance registers as
`yagura` with no environment. Register by hand the same way: pick a name of your own and pass
`YAGURA_INSTANCE` through the CLI's environment flag.

## Windows yagura Opens

**A Claude Code, Codex CLI or GitHub Copilot CLI window that yagura opens from one of its bundled
launch profiles, and a `wsl-muse-code` window or a `muse-code` window on macOS and Linux, needs no
MCP registration.** The daemon points that CLI at the bridge itself:

- At start-up, the daemon looks for `yagura-mcp` next to its own executable. The installer ships
  `yagura-mcp.exe` there, beside `yagura-daemon.exe`. When none is usable, the daemon logs
  `no usable mcp bridge next to the daemon; windows open without MCP injection` and every window
  opens without it.
- Every window's child gets `YAGURA_MCP_BRIDGE`, the bridge's absolute path, beside
  `YAGURA_WINDOW_ID` and `YAGURA_STATUS_TOKEN` -- except the last two in a window whose profile
  hands its credentials over in a file (see below).
- A window launched from a profile with an `[mcp]` table gets that table's arguments appended to
  its command line: Claude Code a `--mcp-config` server definition, Codex CLI `-c` overrides for
  `mcp_servers.<name>` that also forward the channel variables with `env_vars` (and, in
  `wsl-codex-cli`, name them in the server's `WSLENV` so they reach the Windows bridge), GitHub
  Copilot CLI an `--additional-mcp-config` server definition, Muse Code (in WSL, and on macOS and
  Linux) a `-c mcpServers.<name>=...` server definition. The Claude Code, Copilot CLI and Muse Code
  definitions also pass the bridge `--credentials <path>` (the window's credentials file; see
  [Credentials](../profiles/README.md#credentials)). The server
  is named the way Register names it: `yagura`, or `yagura-<instance>` for a
  [named instance](instances.md). The bundled
  `claude-code`, `codex-cli`, `copilot-cli`, `wsl-claude-code`, `wsl-codex-cli`,
  `wsl-copilot-cli` and `wsl-muse-code` profiles carry one, and `muse-code` does on macOS and
  Linux; `shell` does not, `muse-code` does not on Windows (a Muse Code window there reaches yagura
  through the registration described in [Configuring Muse Code CLI](#configuring-muse-code-cli)),
  and `grok-build` and `wsl-grok-build` carry none either: Grok Build has
  no launch argument that adds an MCP server, so its windows reach yagura through the registration
  described in [Configuring Grok Build](#configuring-grok-build). What the table may contain, and when the daemon drops the
  injection instead, is in [profiles/README.md](../profiles/README.md#mcp-injection).

This covers every launch from a profile: `open_window` with `profile`, a `send_handoff` that opens a
new window from a `profile`, `open_task_window`, a restored window reopened from its profile, and
the GUI's add-window form with a profile selected. A window started from a raw command line is not
covered.

The bundled `[mcp]` table applies even when you have changed the profile: your file in the config
root carries only what you changed and is laid over the bundled profile (see
[profiles/README.md](../profiles/README.md#where-profiles-live)).

If the CLI also has a server of that name registered -- `yagura`, or `yagura-<instance>` for a
named instance -- the injected one is what runs in a window yagura opens: Claude Code starts the
`--mcp-config` definition instead of a same-named one from a project's `.mcp.json`, and Codex CLI
merges the `-c` overrides into an existing `[mcp_servers.<name>]` table key by key, keeping the
keys the injection does not set, and GitHub Copilot CLI lets the `--additional-mcp-config`
definition win over its own configuration. A profile of your own that spells `yagura` instead of `{server}`
gives a window of a named instance two servers side by side, and only the injected one can say
which window it is. The bundled
Codex profiles set `tool_timeout_sec` themselves (see [Configuring Codex CLI](#configuring-codex-cli)
for why), so a value in `~/.codex/config.toml` does not apply to a window yagura opens.

**A window turns the other instances' registrations off (Codex CLI).** Codex CLI hands an MCP
server only the variables its own configuration names, so a registered `yagura` that a window of a
named instance can see would start without that window's identity and connect to the default
instance's daemon as an outside client, unable to say which window it is. When a Codex CLI window
opens, the daemon therefore lists the CLI's registrations (`codex mcp list --json`, inside WSL for
`wsl-codex-cli`) and passes `-c mcp_servers.<name>.enabled=false` for each one named `yagura` or
`yagura-<instance>` whose command is this daemon's bridge, except the one the window's own server is
named for. A server of your own is never touched, and the registration itself is not changed: only
that window stops seeing the others. The daemon logs `turned off the other instances' bridge
registrations for this window` with the names it turned off; if the listing fails or takes longer
than 10 seconds it logs a warning and opens the window without turning anything off. In Codex CLI's
`/mcp`, the other instances' servers show as disabled. The bundled `codex-cli` and `wsl-codex-cli`
profiles declare this in [`[mcp_registration.list]`](../profiles/README.md#mcp-registration).

**GitHub Copilot CLI is covered the same way.** It starts an MCP server with the window's whole
environment, but a window would still carry another instance's registered bridge beside its own.
When a GitHub Copilot CLI window opens, the daemon lists its registrations (`copilot mcp list
--json`, inside WSL for `wsl-copilot-cli`) and passes `--disable-mcp-server <name>` for the same
servers, logging and falling back exactly as above. The bundled `copilot-cli` and
`wsl-copilot-cli` profiles declare it too.

**Claude Code is not covered.** It has no documented way to turn a user-scope registration off for
one launch, so a window of a named instance still sees the other instances' registrations. The
default instance's registered `yagura` does no harm there: the server definition yagura hands Claude Code at launch has the same name, and a launch-time definition replaces the user's registration of that name, so the registered bridge is not started.

**Registration is still needed** for a session yagura does not start -- for example the Claude Code
you run in your own terminal to drive yagura's windows -- and for a CLI that cannot be handed an MCP
server at launch, that is, one opened from a profile without an `[mcp]` table.
[Registering from the GUI](#registering-from-the-gui) covers those cases in one place; the
per-CLI sections after it are the manual alternative.

**Status reports come with the same profiles.** The bundled Claude Code profiles also give the CLI
hooks that report each turn's start and end to the daemon, so the window's status no longer depends
on the screen alone. The Codex CLI profiles do the same only once yagura is registered with Codex
CLI, from [Registering from the GUI](#registering-from-the-gui). The GitHub Copilot CLI profiles give the CLI
its hooks at every launch, which it loads once you have answered its folder trust prompt. The
`wsl-muse-code` profile, and `muse-code` on macOS and Linux, give Muse Code its hooks at every
launch; `muse-code` on Windows reports once yagura is registered with Muse Code, and the Grok Build
profiles once yagura is registered with Grok Build.
What the hooks report, and the mark a window
shows when its reports stop arriving, are in
[Detection definitions](../crates/yagura-daemon/detection/README.md#structured-sources---emit-status).

**Delivery comes with the same profiles.** A bundled profile also tells the daemon how its CLI takes the text other windows and the daemon send it -- messages, handoffs, chat and notices -- which is never typed into the CLI's input. How that works, and which windows take part, is in [How the daemon delivers text](#how-the-daemon-delivers-text).

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
`codex-cli`, `copilot-cli`, `muse-code` (on Windows only) and `grok-build` under "This computer",
and `wsl-claude-code`, `wsl-codex-cli`, `wsl-copilot-cli` and `wsl-grok-build` under "WSL".
`shell` and `wsl-muse-code` have no row, and neither does `muse-code` on macOS and Linux, where every launch passes what it needs.

Run the GUI and the daemon from the same release. A daemon of this release lists a row that has
no command of its own (Muse Code), which a GUI from before it cannot read, so its Integrations tab
then reads no row at all.

Each row shows the command it is about to run -- or, for a CLI registered by editing its files
alone (Muse Code), each file it writes and the entries it writes there -- and the CLI's current
state:

- **Registered**: the CLI's own record names this bridge (and, for Codex CLI, carries
  `tool_timeout_sec = 86400`), and the hook settings described below are in place. **Remove** is
  offered.
- **Not registered**: **Register** runs the CLI's own `mcp add`, or writes the files of a CLI
  that has none.
- **Registered to a different bridge**, or **Registered with a different** followed by what
  differs (`tool_timeout_sec`, `args` for the bridge's arguments (shown as **Registered with
  different arguments**), `hooks.state` for Codex CLI's hook trust, or the entries of Muse Code's
  files): **Update** runs `mcp remove`, then the same steps as Register. **Remove** is offered too. Hook entries that are
  only out of date are written again as the tab reads them (see
  [Out-of-date hook entries](#out-of-date-hook-entries) below), so they show here only when that
  could not be done. When Muse Code's settings
  already hold a `managed_hooks_path` of your own, the row says so and Register and Update cannot
  be pressed: yagura does not overwrite it (see
  [Configuring Muse Code CLI](#configuring-muse-code-cli)).
- **Not installed**: the CLI (or, for a WSL row, WSL itself) could not be found or run. Register
  cannot be pressed.
- **Unknown**: the CLI's answer could not be read. It is never guessed as registered or not;
  Register and Remove are both offered.

**What yagura runs and writes.** yagura runs each CLI's own `mcp add` and `mcp remove` -- through
the profile's own launch command, so a WSL row reaches the CLI inside the distribution, with the
bridge's path converted by `wslpath` (the profile's
[`[host_paths]`](../profiles/README.md#host-paths)). Every command a row runs -- `mcp add`,
`mcp remove`, the state read, the path conversion and the file tools -- starts in an empty folder
the daemon keeps for it (`empty`, under the instance's state folder, created again when it is
missing), so no project's settings -- a `.mcp.json`, or the settings a CLI keeps in the folder it
starts in -- are read as the CLI's registration. Your home folder would be such a project, and
from WSL it is the Windows CLI's own settings. When that folder cannot be made, or the daemon found
no home folder, the row's commands are refused. A WSL row whose folder `wsl` cannot translate
starts in the distribution's home instead. When a window opens, the listing that turns off the
other instances' registrations runs in the window's own working directory, so it sees what the
window's CLI sees. Claude Code is registered with `--scope user`. yagura writes these itself,
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
  only while these are in place. Codex hashes each hook's command and attributes, and the hooks
  run `yagura-daemon --hook codex-cli <Event>` (`--hook wsl-codex-cli <Event>` in WSL), which a
  release does not change, so the hashes stay valid across updates. The same write takes out the
  trust of hooks the bundled profile no longer has (its `retired` keys).
- **Muse Code's two files** (Windows only), since Muse Code has no `mcp add`: in
  `%USERPROFILE%\.config\muse\settings.json`, the server entry
  `/mcpServers/<name>` and the two entries that point Muse at a managed hooks file,
  `managed_hooks_path` (`yagura-managed-hooks.json`, beside it) and `managed_hooks_env_vars`; and
  that file, `yagura-managed-hooks.json`, holding the hooks under `/hooks`. Each hook command is
  `if defined YAGURA_WINDOW_ID (set NoDefaultCurrentDirectoryInExePath=1& yagura-daemon ...)`, so
  that a Muse session outside a window runs nothing (see
  [Hooks that run outside a window](../profiles/README.md#hooks-that-run-outside-a-window)).
  Every other entry of either file is kept byte for byte, and a missing file is created.
- **Grok Build's `tool_timeout_sec` and hooks file**: right after `grok mcp add`, it sets
  `tool_timeout_sec = 86400` in the `[mcp_servers.<name>]` table that `grok mcp add` wrote to
  `~/.grok/config.toml`; and it writes a hooks file of the registration's own,
  `~/.grok/hooks/<name>.json`, holding the hooks under `/hooks`. Each hook runs the daemon's copy
  by its absolute path with its arguments apart, so no shell is started, and does nothing outside
  a window (see
  [Hooks that run outside a window](../profiles/README.md#hooks-that-run-outside-a-window)). The row
  shows the profile's notice about folder trust (see [Configuring Grok Build](#configuring-grok-build)).

**Remove** takes the hook trust tables (the `retired` ones included) and the hooks entry out again,
along with the CLI's own `mcp remove`; it leaves `tool_timeout_sec` alone. For Muse Code it takes out the instance's server
entry, and the shared hooks entries only with the last instance's registration (see
[Shared entries and the claim](../profiles/README.md#mcp-registration)); the files stay. Any failed step stops the operation there. The output
of `add` and `remove` is shown in the row; what a state read returns is neither shown nor logged.

**Update and Remove discard what you wrote by hand in Codex CLI's table.** `codex mcp remove`
deletes the whole `[mcp_servers.<name>]` table, and `codex mcp add` rewrites it, so any key you
added to it yourself -- such as `env_vars` -- is gone afterwards; yagura writes back only
`tool_timeout_sec`. Add such keys again after an Update.

**Reading the state.** The rows are read when the tab opens and after each operation, never in the
background, and a row's out-of-date hook entries are written again as it is read (see
[Out-of-date hook entries](#out-of-date-hook-entries)): `claude mcp get <name>` for Claude Code, `codex mcp get <name> --json` for Codex CLI,
`copilot mcp get <name> --json` for GitHub Copilot CLI, Muse Code's two files above, and
`grok mcp list --json` for Grok Build, plus the hook settings above, read back from
`~/.codex/config.toml` and `~/.grok/hooks/<name>.json`. `claude mcp get`
starts the bridge briefly to check that it connects; that connection is `External` and changes no
window's `mcp_connected`.

**A named instance** registers as `yagura-<instance>` and carries `YAGURA_INSTANCE` (see
[Connecting to a Second Instance](#connecting-to-a-second-instance)).

**A CLI session that is already running** does not see a change until it is restarted.

Only the GUI can do this: the daemon accepts these requests from a registered approver alone (see
[Approver Registration](#approver-registration)), and there is no MCP tool for them. When the
daemon has no bridge beside it, the tab says so and reads no row.

### Out-of-date hook entries

The hooks a registration writes -- Grok Build's hooks file, Muse Code's managed hooks, and Codex
CLI's hook trust -- stay in the CLI's settings after an update. They
run `yagura-daemon --hook <profile-id> <Event>`, and what each event does is read from the profile
when the hook runs (see [Hook events](../profiles/README.md#hook-events)), so an update rarely
changes them. When it does, or when they were written by 0.3.0 or earlier, yagura writes them
again itself; there is no need to press Update.

- **When**: each time a window opens from the profile, and each time the Integrations tab reads
  the rows -- for a row the CLI lists as registered, or registered with a difference. A row that
  reads **Not registered** (for example after `grok mcp remove` by hand) is left as it is, and its
  hook entries are written again when its next window opens. The row then still reads **Not
  registered**: yagura does not run `mcp add` for you.
- **Only with your consent**: only while this instance is still registered there, judged by what
  this instance's **Remove** would take out -- `/mcpServers/<name>` for Muse Code, the hooks in
  `~/.grok/hooks/<name>.json` for Grok Build, and `[mcp_servers.<name>]` in `~/.codex/config.toml` for Codex CLI. After a Remove nothing is
  written, and an instance you never registered gets nothing. Checking this uses only the reads
  of these files that opening a window or reading the row already makes, and starts no other
  process.
- **What**: only the entries the registration owns, written where Register writes them, and Codex
  CLI's retired trust taken out. It does not run `mcp add` or `mcp remove`, does not set
  `tool_timeout_sec`, and changes nothing else in the file. For Muse Code the server entry is
  among those entries, so it is written again too. Nothing is written while Muse Code's settings
  hold a `managed_hooks_path` of your own.
- **What you see**: the daemon's log has one line for each rewrite, and the row reads
  **Registered** -- or still shows what differs outside the hooks, such as a different bridge.
- **When it cannot be done** (a file that cannot be read or written, or a Register, Update or
  Remove of yours that is still running past the listing's time limit): the window opens without
  its hooks, as before, and the daemon's log says why, with "register the CLI again to restore
  them"; the row shows what differs, and **Update** puts it right.

**Running an instance of 0.3.0 or earlier beside this release, as the same user.** Muse Code's
managed hooks and Codex CLI's hook trust are shared by every instance. Once this release writes them
in the `--hook` form, the older instance's daemon cannot read them: its `muse-code` windows report a
failed hook, and it opens its Codex CLI windows without hooks. Update the older instance to put it
right. Grok Build keeps the entries of each instance apart, and is not affected.

## Configuring Claude Code

This section and the per-CLI sections after it are the manual alternative to
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
`mcp add` / `mcp remove` and the entries that section lists.
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

2. Codex CLI's own per-call tool timeout (`tool_timeout_sec`, default 60 seconds) is shorter than the `timeout_sec` that [`wait_for_status`](#wait_for_status) and [`wait_for_report`](#wait_for_report) accept, and than a gated call can wait for its approver. So lift Codex's own limit out of the way rather than tuning it to these waits. Edit `~/.codex/config.toml`:

   ```toml
   [mcp_servers.yagura]
   command = "<absolute path to yagura-mcp>"
   tool_timeout_sec = 86400  # a day: well beyond any wait the daemon bounds,
                             # the same value the bundled Codex profiles pass
   ```

3. **A registration needs no `env_vars`.** A session you start yourself runs outside any window, so it has no window ID or token to forward, and a named instance's `YAGURA_INSTANCE` travels in the registration's own environment instead (`codex mcp add --env YAGURA_INSTANCE=<instance> ...`, which writes `[mcp_servers.<name>.env]`; see [Connecting to a Second Instance](#connecting-to-a-second-instance)). A window yagura opens from a bundled Codex profile receives `env_vars` at launch through its `[mcp]` injection (see [Windows yagura Opens](#windows-yagura-opens)). `env_vars` matters only for Codex CLI started *inside* a yagura window without that injection -- from a raw command line, or a profile with no `[mcp]` table. Codex CLI does not hand its own environment to a stdio MCP server: it builds the server's environment from a fixed allow-list (`WINDOWS_CORE_ENV_VARS` on Windows, `HOME` / `PATH` and friends on Unix) plus whatever `[mcp_servers.<id>].env` and `env_vars` name, so the `YAGURA_WINDOW_ID` / `YAGURA_STATUS_TOKEN` that the daemon injected into the window's process tree never reach `yagura-mcp` on their own. Every call made from such a window is then `External` (see [Requester Identification](#requester-identification)): it always raises a human approval dialog, its `send_handoff` envelope carries `"from":"external"` with no `reply` key to answer on, and even a `send_message` aimed at that same window is not recognized as self-addressed. To have it identify as its window, add `env_vars = ["YAGURA_WINDOW_ID", "YAGURA_STATUS_TOKEN", "YAGURA_INSTANCE"]` to the table by hand -- and again after any Update from the GUI, which discards it (see [Registering from the GUI](#registering-from-the-gui)).
4. Run `codex mcp list` to confirm the server is registered and reachable.
5. `codex` itself must be resolvable on `PATH` for a `send_handoff` targeting a `codex-cli` launch profile to spawn successfully -- a `profile` whose command cannot be found fails later, at spawn, as `spawn_failed`. On Windows it must be Codex CLI's standalone build, not the npm package: a profile that resolves to the npm package's `.cmd` shim is listed as unavailable (see [How the daemon delivers text](#how-the-daemon-delivers-text)).
6. **Codex's own trust confirmation is not detected by yagura.** The first time Codex CLI runs in a working directory it has not seen before, it shows a directory/hooks trust dialog on its own terminal; yagura's shipped detection definitions do not recognize this prompt, so the window's status stays `unknown` forever, and the daemon hands nothing to an `unknown` window: what it sends waits in the window's inbox, and a handoff addressed to it is refused as `status_unknown`. Launch Codex by hand once in whatever `cwd` a window will run in, and clear that dialog, before relying on delivery to reach it.

## Configuring GitHub Copilot CLI

A GitHub Copilot CLI window yagura opens from the bundled `copilot-cli` or `wsl-copilot-cli` profile
already reaches yagura (see [Windows yagura Opens](#windows-yagura-opens)). Register yagura for any
other Copilot session -- from [Registering from the GUI](#registering-from-the-gui), or by hand
(GitHub Copilot CLI 1.0.101):

```bash
copilot mcp add --timeout 86400000 yagura -- <absolute path to yagura-mcp>
```

`copilot mcp get yagura` shows the registration, and `copilot mcp remove yagura` undoes it.
`--timeout` (milliseconds) matters: Copilot ends an MCP tool call after 180 seconds unless the
server's entry sets a longer limit, and yagura's `wait_for_status` and `send_message` can wait
longer than that. The windows yagura opens and a registration from the GUI both set one day; a
registration by hand needs the flag above.

1. **`copilot` must be on `PATH`.** `winget install GitHub.Copilot` adds its package folder to your
   user `PATH`; a process that was already running, the daemon among them, does not see the change,
   so restart yagura after installing. For `wsl-copilot-cli`, install the Linux build inside the
   distribution (see [docs/wsl.md](wsl.md)).
2. **On Windows, the hooks need PowerShell 7 (`pwsh`) on `PATH`.** Copilot CLI runs a hook's
   Windows command with it.
3. **yagura does not answer Copilot's folder trust prompt.** In a folder Copilot has not been told
   to trust, it asks first, and the window shows `waiting_approval` until you choose on its screen.
   Copilot loads the profile's hooks and sends a request the window was started with only after
   that; choose `2. Yes, and remember this folder for future sessions` so it does not ask there
   again.
4. **The start-up context arrives with the first prompt.** Copilot runs its session-start hook when
   the first prompt is sent, not when the window opens.
5. **After Esc twice the window stays `running` for a while.** Copilot runs no hook when a turn is
   interrupted. A Ctrl+C typed in the window, or the window's interrupt button, ends the turn at
   once, as the screen shows, on a version whose footer shows `ctrl+c interrupt` while a turn runs
   (1.0.112). A turn cancelled with Esc twice is not ended by the key: the window reads as
   `waiting_input` once its output has been quiet for 5 seconds, and later while Copilot keeps
   redrawing its screen. On a version that never shows `ctrl+c interrupt` (1.0.101), this is so for
   Ctrl+C as well. See the
   [detection limits](../crates/yagura-daemon/detection/README.md#limits).
6. **Copilot reads a project's Claude Code settings too.** It takes the working folder's `.mcp.json`
   and `.claude/settings.json` as its own configuration, so the MCP servers and hooks set up there
   for Claude Code also apply to a Copilot window opened in that folder.

## Configuring Muse Code CLI

Muse Code (`muse`, checked with 1.4.12) has no `mcp add` command, and two bundled profiles reach it
in different ways:

- **`muse-code` on Windows** passes nothing at launch, since Muse's Windows launcher cannot carry
  the arguments. Register yagura with Muse Code once, from the Muse Code row in
  [Registering from the GUI](#registering-from-the-gui): that writes yagura's server and hooks
  into Muse's own settings, which every Muse session reads. Until then a `muse-code` window opens,
  but has no yagura MCP tools, reports nothing and takes no part in chat or handoffs.
- **`muse-code` on macOS and Linux**, and **`wsl-muse-code`** (Windows, running Muse inside WSL),
  pass the server and a managed hooks file with `-c` at every launch, and register nothing: Muse's
  settings are not changed, and there is no Muse Code row to register from.

To register by hand on Windows instead, write the entries the `muse-code` profile's
`[mcp_registration]` lists, with the bridge's absolute path for `{bridge}`.

`muse` must be on the daemon's `PATH`. On Windows its installer adds its folder to your user `PATH`;
a daemon that was already running does not see the change, so restart yagura after installing. For
`wsl-muse-code`, install the Linux build inside the distribution (see [docs/wsl.md](wsl.md)).

**Why the hooks go in a managed hooks file.** (This is about a `muse-code` window on Windows, whose
hooks are written once for all windows. A window launched with its own hooks file -- `muse-code` on
macOS and Linux, and `wsl-muse-code` -- has commands that carry `--credentials` instead, so
`managed_hooks_env_vars` there lists only `YAGURA_INSTANCE`.) Muse clears a hook's environment down to a fixed list
of variables, so a hook it runs from your own or a project's settings never sees
`YAGURA_WINDOW_ID` or `YAGURA_STATUS_TOKEN` and cannot report for its window. Only the one file
named by `managed_hooks_path` also receives the variables `managed_hooks_env_vars` lists, so
yagura's hooks are put there. Muse passes an MCP server only the variables its definition names,
which is why the server's `env` names them too.

### Muse Code limits

1. **On Windows, `muse-code` cannot take a start-up request.** Muse's Windows launcher
   (`muse.cmd`, which runs PowerShell 5.1) drops `"` and `--` from its arguments. So a
   `send_handoff` that opens a new window and an `open_task_window` for a task with an instruction
   are refused as `unsupported` before approval, and the GUI shows the same limit before a window
   is opened (see [Shim guidance](../profiles/README.md#shim-guidance)). `wsl-muse-code` takes
   both. This is to be measured again once Muse's Windows entry point passes `"`, `--` and any
   other argument through unchanged.
2. **With two instances registered, every Muse session starts both instances' servers** (`muse-code`
   on Windows only). Each entry names its instance by value, so in a window of one instance the other
   instance's server takes the window's variables to its own daemon, where they name no window,
   and it works there as a client from outside a window. Muse refuses to start when a launch
   argument touches a server its settings define, so there is no way to switch it off for one
   window. A Muse you start yourself outside yagura reaches each registered instance the same way.
3. **Where every launch passes the server, a server of your own named `yagura`** (or
   `yagura-<instance>`) in `~/.config/muse/settings.json` -- the distribution's, for
   `wsl-muse-code` -- would make Muse refuse the launch, so the window opens without yagura's server
   and the daemon logs one warning saying so. A registration an earlier release wrote on macOS or
   Linux is such a server: remove `mcpServers.yagura` (and `managed_hooks_path`,
   `managed_hooks_env_vars` and `yagura-managed-hooks.json`, if they are yagura's) from those
   settings by hand, since there is no Muse Code row to remove it from any more.
4. **Where every launch passes the hooks, your own `managed_hooks_path` is replaced in yagura's
   windows.** The window's `-c`
   points Muse at yagura's hooks file for that window only, so your managed hooks do not run there.
   Your settings file is not changed, and Muse sessions outside yagura are unaffected. On Windows,
   Register is refused while your settings hold a `managed_hooks_path` of your own (see
   [Registering from the GUI](#registering-from-the-gui)).
5. **`XDG_CONFIG_HOME` is not supported.** yagura always reads and writes `~/.config/muse`
   (`%USERPROFILE%\.config\muse` on Windows), and the `wsl-muse-code` checks read the same place
   inside the distribution.
6. **Where every launch passes the server, the MCP call time limit cannot be raised.** Muse's `-c`
   refuses `tool_timeout_sec`, so a `wsl-muse-code` window, and a `muse-code` window on macOS and
   Linux, runs a long `wait_for_status`, `wait_for_report` or `send_message` under
   Muse's own default limit, 5 minutes in Muse Code 1.4.12 to 1.4.14: ask for a shorter `timeout_sec`, or wait
   again. A call Muse gives up on ends when its turn ends: a wait answers as if its time had run
   out, and a `wait_for_report` takes no report, so one that arrives later waits for the next call.
   A registration sets `tool_timeout_sec = 86400` for `muse-code` on Windows.
7. **Interrupting before the first output puts the text back in the input line** (see
   [Limits](../crates/yagura-daemon/detection/README.md#limits)).
8. **Muse may ask before each call to a yagura tool**, depending on its own automatic approval
   (`--approval-judge`). A call waits on that dialog on the window's screen until it is answered.
9. **yagura does not answer Muse's folder trust prompt.** In a folder Muse has not been told to
   trust, a window starts with it and shows `waiting_approval`; choose `Trust and continue` on its
   screen.
10. **Neither Muse profile takes an effort level.** `--reasoning-effort` is saved as your own
    default in Muse's settings, and the one-launch override `-c reasoning_effort=...` is ignored
    unless your settings already hold `reasoning_effort` (Muse Code 1.4.12 to 1.4.14), so yagura has no way
    to set it for one window and refuses `effort` for both profiles. Set it in Muse itself. This
    is to be measured again with each Muse release.
11. **A Muse whose `max_consecutive_stop_hook_continuations` is 0 ignores a stop's block.** What
    waits in a window's inbox is handed over at the end of a turn as a block of its `Stop` hook,
    which Muse follows only while that setting lets it continue the turn. Set to 0, Muse drops the
    block, so a text handed over that way counts as delivered although the model never reads it
    (see [How the daemon delivers text](#how-the-daemon-delivers-text)). Leave the setting above 0.
12. **An AI's interrupt stops Muse at its next tool result.** `interrupt_window` from an AI is
    answered at Muse's `PostToolUse` and `PostToolUseFailure` hooks, so a turn that calls no tool
    runs to its end, and an instruction sent after the stop reaches Muse only at your next turn
    (see [interrupt_window](#interrupt_window)).

Run the GUI and the daemon from the same release (see
[Registering from the GUI](#registering-from-the-gui)).

## Configuring Grok Build

Grok Build (`grok`, checked with 1.0.70 on Windows and in WSL) has no launch argument that adds an
MCP server or a hook, so both bundled profiles -- `grok-build` (Windows, macOS and Linux) and
`wsl-grok-build` (Windows, running Grok Build inside WSL) -- reach yagura through a registration.
Register yagura with Grok Build once, from its row in
[Registering from the GUI](#registering-from-the-gui): that runs `grok mcp add` with the bridge's
absolute path, sets `tool_timeout_sec`, and writes the hooks file `~/.grok/hooks/<name>.json`. Until
then a Grok Build window opens, but has no yagura MCP tools, reports nothing and takes no part in
chat or handoffs. To register by hand instead, run `grok mcp add yagura -- <absolute path to
yagura-mcp>` and write the entries the `grok-build` profile's `[mcp_registration]` lists.

`grok` must be on the daemon's `PATH`; a daemon that was already running when it was installed does
not see it, so restart yagura after installing. For `wsl-grok-build`, install the Linux build inside
the distribution (see [docs/wsl.md](wsl.md)). Sign in once in a terminal of your own with `grok login
--oauth` before opening a window.

**What registering changes for every Grok Build session.** The row shows it before you press
Register: while your own Grok Build settings hold a hook, as they do once yagura is registered,
Grok Build asks whether to trust every folder it starts in, except your home directory and the root
of a drive or file system, and that includes the sessions you start yourself. Answering yes trusts the whole folder, so its own hooks, MCP
servers and instructions run too. yagura does not answer the prompt: a window that starts on it
shows `waiting_approval` until you choose on its screen, and its hooks run only after that.

1. **Claude Code's and Cursor's hooks and MCP servers are switched off in yagura's windows.** Grok
   Build runs them as well as its own; the profiles set `GROK_CLAUDE_HOOKS_ENABLED`,
   `GROK_CLAUDE_MCPS_ENABLED`, `GROK_CURSOR_HOOKS_ENABLED` and `GROK_CURSOR_MCPS_ENABLED` to `false`,
   so a window runs only what is registered with Grok Build. Skills, rules and agents are still
   read. A Grok Build you start yourself is unaffected.
2. **A trusted folder's `.mcp.json` is read all the same.** Those variables do not turn it off, so an
   MCP server it lists, a `yagura` one included, also runs in a window opened in that folder; Grok
   Build's own registration of a name is used over one of the same name there. yagura does not
   change a project's files.
3. **Another instance's yagura is listed in a window but cannot be called.** Every Grok Build
   session starts every registered server, and Grok Build has no launch argument that leaves one
   out. A window therefore refuses the tools of every other instance's registration (`--deny
   MCPTool(<name>__*)`); their names still show in Grok Build's tool search, and a call is refused
   by its permission policy. This is to be changed once Grok Build has a way to choose a session's
   MCP servers at launch.
4. **The model is set for one launch through the environment.** `-m` would save it as your own
   default, so the profiles pass `GROK_DEFAULT_MODEL` instead (see
   [Launch options](../profiles/README.md#launch-options)). An `effort` level the model does not
   take is not refused: Grok Build falls back to the model's default level without a word.
5. **The room's introduction arrives with the launch.** Grok Build passes no session-start hook's
   output to its model, so the profiles hand it the introduction with `--rules`, and the window takes
   waiting texts after a tool call, or at the end of a turn (see
   [How the daemon delivers text](#how-the-daemon-delivers-text)).
6. **Remove the registration before uninstalling yagura.** Uninstalling changes none of Grok Build's
   files, so a hooks file left in `~/.grok/hooks` keeps naming a daemon copy that may be gone, and
   keeps Grok Build asking about folder trust.
7. **After your own interrupt (Ctrl+C), Grok Build's history holds the focus** and the window reads
   as `unknown` until the focus is back in the input line (see
   [Limits](../crates/yagura-daemon/detection/README.md#limits)).
8. **An instruction sent after an AI's interrupt waits for your next turn.** An AI's
   `interrupt_window` stops Grok Build at its next tool call through its hooks, and Grok Build has
   no hook or command through which a text could reach the stopped session before you send it
   something yourself (see [interrupt_window](#interrupt_window)).

## Removing yagura from Antigravity CLI

yagura no longer supports Antigravity CLI (`agy`; see
[Antigravity CLI is not supported](../README.md#antigravity-cli-is-not-supported)), so it has no
Integrations row for it, and it does not start `agy` to take out what an earlier release
registered. If you registered yagura with Antigravity CLI, remove it yourself, at your own
terminal:

1. **The hooks entry.** In `~/.gemini/config/hooks.json`, delete the entry named `yagura` -- or
   `yagura-<instance>` for a [named instance](instances.md) -- and leave the other entries as they
   are.
2. **The MCP registration.** Run `agy mcp remove yagura` (`agy mcp remove yagura-<instance>` for
   a named instance), or delete that server from `mcpServers` in `~/.gemini/config/mcp_config.json`.
3. **WSL.** A registration made from a WSL row lives in the distribution's own home: do both
   steps again inside it, on its `~/.gemini`.

Left in place, the hooks entry runs in every `agy` session. Outside a yagura window it does
nothing, and inside one it does nothing either, at most leaving a line in the daemon's log; once
yagura is uninstalled, the entry names a daemon that is gone, and `agy` reports a failed hook. The
MCP registration keeps showing yagura's tools to an `agy` you start yourself, as an `External`
requester (see [Requester Identification](#requester-identification)).

## How the daemon delivers text

Nothing a window is sent is typed into its CLI's input. A [`send_message`](#send_message), a [`send_handoff`](#send_handoff) to an existing window, a chat post, the chat announcement, the notice a task window starts with, the plan-approved notice and the report notice all go into the window's *inbox*, and the CLI takes them through a door its own extension points open: a hook, the request it is started with, or a command that queues a message on its session. A CLI answers a typed line as a prompt from its user, so typing text nobody wrote into it would leave what the CLI does with it to the model; a door hands it over as text the CLI's own hook or launch supplied.

**Doors.** A window's CLI runs these commands from its hooks. Like `yagura-daemon --emit-status` (see [StatusEvent Authentication](#statusevent-authentication)), each is bound to its window by `YAGURA_WINDOW_ID` and `YAGURA_STATUS_TOKEN` -- or by the credentials file named by `--credentials <path>`, in a window whose profile keeps them out of the environment -- does nothing outside a yagura window (an empty value counts as unset), and can read only its own window's inbox.

| Command | Run by | What it does |
| --- | --- | --- |
| `yagura-daemon --deliver-context` | the CLI's session-start hook | prints the chat announcement and whatever waits in the inbox |
| `yagura-daemon --deliver-take` | the hook that runs at the start of a turn (for Grok Build and GitHub Copilot CLI, after a tool call) | prints whatever waits and takes it out of the inbox |
| `yagura-daemon --deliver-wait` | a hook the CLI runs in the background | waits until something arrives for an idle session, prints it, and ends with the exit code that wakes the CLI; only one such wait runs per window |
| `yagura-daemon --report-session` | a hook that receives the CLI's session identifier | tells the daemon which session this window's CLI is, so it can be addressed later |
| `yagura-daemon --hook <profile-id> <Event>` | any hook of a profile with a `[hook_events]` table | does what the named profile's `[hook_events]` says for the event -- report a status, record the session, print the context, take what waits, or hand it over as the block of a stop -- reading the profile each time it runs (see [Hook events](../profiles/README.md#hook-events)) |
| `yagura-daemon --print-answer <Event>` | a hook whose answer the window's open wrote beside its credentials file | prints that answer without connecting to the daemon, so it answers while no daemon runs (see [`[hooks]` answers](../profiles/README.md#answers-a-hook-prints-without-the-daemon)) |

**A door prints first and confirms after.** A door tells the daemon it printed what it was handed only once its output is written; an envelope counts as delivered then, and not before. A door that ends without confirming -- its CLI cut it off, or the daemon ended meanwhile -- leaves what it carried waiting in the inbox again, so the next door, or a later daemon, hands it over again. A CLI can therefore be handed the same envelope twice; every envelope carries an `id`, so act once on an `id` you were handed before.

Each answer carries at most the profile's `max_bytes` (see [Delivery](../profiles/README.md#delivery)), counted in the form the door prints. A text is never cut: the texts that do not fit wait for the next answer, and a text that would not fit even an answer of its own is refused when it is sent (`send_message` answers `invalid_argument`).

**When each text arrives.**

| The window is | How it arrives |
| --- | --- |
| just opened | the session-start door prints the chat announcement and anything waiting; a CLI whose profile declares `launch_context` is given the announcement on its command line instead, and what waits stays in the inbox. A task window, and a window a handoff opens from a profile, are started with their text as the CLI's first request instead (the profile's `launch_prompt`): a request the parent actually made |
| not yet at its first turn | it waits in the inbox; the first turn's door hands it over |
| working | it waits; the next door of the turn hands it over: the start of the next turn, or, for a CLI whose hooks take the inbox after a tool call, the next tool call |
| ending a turn | for a CLI whose stop hook hands over a block (`deliver = "block"`), everything waiting is handed over in one block when the turn stops, which makes the CLI go on with it in the same session. The bundled profiles hand one over only at a stop that is not itself the continuation of a block (the event's `when`), so a CLI is never kept going by one block after another |
| waiting for input | the profile's `wake` decides: `wait` ends the wait the CLI left registered, `command` runs the CLI's own program to queue the text on its session (needs the session identifier a hook reported, so after the first turn), and `none` leaves it for the CLI's next door -- its next turn, such as when you next send a prompt in the window |
| `unknown` | nothing is pushed; texts wait for the CLI's next door |

**When a waiting CLI has no wait registered.** A Claude Code window is woken through a wait its CLI leaves registered (`wake = "wait"`). Such a wait can be gone while the CLI waits for input: the daemon ended abnormally and the window outlived it, or the CLI's own time limit for the wait ran out. The window's open therefore names a *wake file*, which the CLI watches: when something waits, the window is not `unknown` or `exited`, and no wait is registered, the daemon writes the file once, and its change makes the CLI register a wait again, which then takes what waits. The daemon writes it again only after one of the window's doors has run, a wait was registered, or the window's status changed. The list of files the CLI watches is handed to it at session start and on each change of directory by `--print-answer`, which needs no daemon, so a directory change while no daemon runs keeps it. A window opened before this release, or by an older daemon, has no wake file: what waits there arrives at the CLI's next turn, as the `note` says. `yagura-daemon --shutdown` closes every window, so no wait outlives it; this applies to a daemon that ends without closing its windows.

A text counts as delivered when a door printed it and said so, or the wake command succeeded. The bundled profiles:

| Profile | Start-up and turn doors | At the end of a turn | Waking a waiting CLI |
| --- | --- | --- | --- |
| `claude-code`, `wsl-claude-code` | `SessionStart` and `UserPromptSubmit` hooks, injected with `--settings` | -- | a background `Stop`/`SessionStart` hook that ends with exit code 2 when something arrives, registered again through the wake file (`FileChanged`) when it is gone |
| `codex-cli`, `wsl-codex-cli` | `SessionStart` and `UserPromptSubmit` hooks, injected with `-c` | -- | `codex queue --thread <session> --message <text>`, run by the daemon |
| `copilot-cli`, `wsl-copilot-cli` | a `sessionStart` hook in a plugin directory, injected with `--plugin-dir`, which runs when the first prompt is sent; `postToolUse` takes the inbox after each tool call, since Copilot discards a command hook's output at the start of a turn | a block from `agentStop` | none |
| `muse-code`, `wsl-muse-code` | `SessionStart` and `UserPromptSubmit` hooks in a managed hooks file (`SessionStart` runs when the first prompt is sent); see [Configuring Muse Code CLI](#configuring-muse-code-cli) for how each profile places it | a block from `Stop` | none |
| `grok-build`, `wsl-grok-build` | no start-up door: the chat announcement is given with `--rules` when the window opens (`launch_context`). `PostToolUse` and `PostToolUseFailure` hooks in `~/.grok/hooks/<name>.json`, written by registration, take the inbox after each tool call; Grok Build passes a hook's output to its model only from the tool-call hooks and the stop's block | a block from `Stop` | none |

What each profile declares is in [`[delivery]`](../profiles/README.md#delivery).

**Which windows take part.** A window is handed text this way only if it was opened from a profile that declares `[delivery]` and its hooks were put in place when it opened. A window that was not -- a plain shell, a window opened with a raw `command`, a profile with no `[delivery]` table -- is not a chat participant, gets no announcement, and a `send_message` or `send_handoff` addressed to it is answered `undelivered_no_delivery`. The bundled Codex profiles inject their hooks only once yagura is registered with Codex CLI (from [Registering from the GUI](#registering-from-the-gui)), because Codex runs a command-line hook only once its trust is recorded; a `muse-code` window on Windows and a Grok Build window take part only while the hooks their registration writes are in place.

**Bounds.** A window's inbox is bounded; past it a chat post is `undelivered_exhausted`, and a `send_message` or `send_handoff` is `resource_exhausted`. One sender -- a window, or every caller that is no window, together -- may leave at most `[limits] max_queued_per_sender` envelopes (8 by default) waiting in one window; one more is `resource_exhausted`, and a slot comes free once one of them is handed over or withdrawn, the window ends, or the daemon restarts. When a window ends, the envelopes it left waiting in every other window are taken out; one a door is already carrying is left to it. A message or handoff still waiting when the daemon stops is lost (its `note` says so), and a chat post still waiting for a window that does not come back reads as `undelivered_restart`.

**Windows and Codex CLI.** The wake command passes the text as a command-line argument, which the Windows command interpreter would reinterpret if Codex CLI were started through the npm package's `.cmd` shim. A `codex-cli` profile that resolves to a `.cmd` file is therefore listed as unavailable, with a reason that points to the standalone build; install Codex CLI's standalone build and make sure it comes first on `PATH` (see [Availability](../profiles/README.md#availability)).

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

`queued_count` is how many envelopes and notices wait in the window's inbox (see [How the daemon delivers text](#how-the-daemon-delivers-text)), whoever sent them. It is omitted when the daemon does not report it.

The GUI shows the count as an envelope mark with the number, in the window's tab and in the title bar of a floating window, with the tooltip "N queued message(s) — delivered on its next turn". No mark is shown when the count is 0 or when the daemon does not report it.

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

The `last_output_summary` contains up to 500 characters from the end of the text the window's screen shows, read the way [`read_output`](#read_output) reads it (a line longer than the window is wide is one line) but without the rows that scrolled off its top. `mcp_connected` and `queued_count` mean the same as in [list_windows](#list_windows).

`queued_ids` lists the `id`s of the envelopes you sent with [`send_message`](#send_message) or [`send_handoff`](#send_handoff) that still wait in this window's inbox: those your own window sent, or, for a caller that is no window, those the same MCP session sent while its connection to the daemon lasts (an IPC client: the same connection). An `id` that no longer waits was handed over or is being carried by a door, or was withdrawn, or the window or the daemon ended meanwhile: an envelope lost to a daemon restart no longer waits either, so read the window before you take its absence as delivery.

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

Put text in a window's inbox as one envelope. Nothing is typed into the window: its CLI takes the envelope through its own doors, as described in [How the daemon delivers text](#how-the-daemon-delivers-text).

**Input:**
```json
{
  "window_id": "w1-k3f9xa",
  "text": "Please rerun the tests on your branch."
}
```

**Output:**
```json
{
  "window_id": "w1-k3f9xa",
  "id": "2f0c9a1e7b3d4c58e1a0b9c8",
  "state": "queued",
  "note": "No wait of the window's CLI is registered right now; it arrives at the CLI's next turn. It is lost if the daemon restarts before then.",
  "requester": "window"
}
```

**What the CLI gets** is one line: the fixed prefix `[yagura message] ` followed by a single-line JSON object with the keys `id`, `from`, `text` and `reply`. `from` and `reply` follow [`send_handoff`'s envelope](#send_handoff): `reply.window_id` is where to answer, with another `send_message`, and a caller outside any window is `"from":"external"` with no `reply`. The text is data, not instructions: a receiving agent must treat it as a request to consider, never as a command to obey.

`text` may run over several lines, and its line endings are kept as written: inside the envelope a line ending is data. Any other control character (for example `ESC`), and any character that could reorder or hide text, is rejected as `invalid_argument`, and so is a text longer than one answer of the window's doors carries (the profile's `max_bytes`, see [Delivery](../profiles/README.md#delivery)); both are checked before any approval decision is made. When the call is held for a decision, a body over 2048 bytes is also rejected as `invalid_argument`. To hand a window a long request, put it in a plan task's `instruction` (see [create_plan](#create_plan)) and open the task's window with [open_task_window](#open_task_window).

**`state`** says what became of the envelope:

- `"delivered"` -- the CLI was handed the envelope: a door printed it and said so, or the wake command that carries it succeeded. It says the CLI received the text, not that the model read it or acted on it.
- `"queued"` -- it waits in the window's inbox. `note` says when it is handed over and what could lose it on the way, read off the window as it is now (for example, that the window's CLI has no wait registered, so it arrives at the CLI's next turn). Its `id` is in [`get_window_status`](#get_window_status)'s `queued_ids` for as long as it waits, and [`withdraw_message`](#withdraw_message) takes it back.
- `"undelivered_no_delivery"` -- the window was not opened from a profile that declares how its CLI is handed text, or the profile's hooks were not put in place when it was opened. Nothing was sent. This is answered before the approval decision, since it could not change it. To run a command, open a window with it as [`open_window`](#open_window)'s `command`.

The call waits only while a wait the CLI left registered, or its wake command, carries the envelope; it never waits for the window's next turn. **Act once per `id`**: an envelope a door was carrying when its CLI cut it off, or when the daemon ended, is handed over again, so a CLI can be handed the same `id` twice. A sender that resends after an error should say in the text that it is a resend, since the earlier one may already have reached the receiver under another `id`.

`send_message` is a gated call (see [Approval Flow](#approval-flow)): it succeeds immediately when the caller is the target window itself or a registered approver, and is otherwise held pending a decision unless the target's status is `unknown`, in which case it is refused outright as `status_unknown` -- even for a caller sending to its own window. A window whose detection definition never matches its own output stays `unknown` forever, so it can never receive `send_message` at all (see [Troubleshooting `status_unknown` and `child_exited`](#troubleshooting-status_unknown-and-child_exited) for workarounds). A denial or timeout is reported as `approval_denied` or `approval_timeout`, and no approver being connected is reported as `approval_required`. A call held for a decision is withdrawn as `requester_interrupted` when an AI interrupts the caller's own window (see [interrupt_window](#interrupt_window)).

**Bounds**: one sender may leave at most `[limits] max_queued_per_sender` envelopes (8 by default) waiting in one window, and the window's inbox is bounded as a whole; past either the call is `resource_exhausted`. A window that sends and then ends takes the envelopes it left waiting with it. See [How the daemon delivers text](#how-the-daemon-delivers-text).

### withdraw_message

Take back an envelope you sent with [`send_message`](#send_message) or [`send_handoff`](#send_handoff) while it still waits in its window's inbox; it then never reaches the window's CLI.

**Input:**
```json
{
  "id": "2f0c9a1e7b3d4c58e1a0b9c8"
}
```

**Output:**
```json
{
  "id": "2f0c9a1e7b3d4c58e1a0b9c8",
  "outcome": "withdrawn"
}
```

`outcome` is one of:

- `"withdrawn"` -- it was waiting and is out of the inbox.
- `"under_way"` -- a door or a push is carrying it now. It waits again if they end without handing it over, and can then be withdrawn.
- `"not_waiting"` -- no window holds an envelope of that `id` that you sent: its CLI was handed it, it was withdrawn already, its window ended, or the daemon restarted. The answer does not tell these apart; [`get_window_status`](#get_window_status)'s `queued_ids` shows which of your envelopes still wait.

Only the envelope's sender may withdraw it: the window that sent it, or, for a caller that is no window, the same MCP session that was answered with its `id`, for as long as its connection to the daemon lasts (an IPC client: the same connection); anyone else is answered `not_waiting`. Once that session ends, or the relay reconnects to the daemon, its calls come from a new caller, which is answered `not_waiting` for the envelopes sent before. No approval is asked. Chat posts are not envelopes and cannot be withdrawn.

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
  "name": "codex-reviewer",
  "opened": true,
  "id": "9d4e1b7c0a3f5e2d8c6b4a19",
  "state": "delivered",
  "note": null,
  "requester": "window"
}
```

`name` is the name of the window that received the envelope. A `name` you send with `profile` follows the rules [`open_window` documents](#open_window): a name a running window already carries is refused as `name_taken`, and when you send none the profile's `display_name` is numbered if taken.

`window_id` and `profile` are mutually exclusive, and exactly one of them must be present -- omitting both, or sending both, is `invalid_argument`. `cwd` and `name` apply only to the `profile` form and are rejected alongside `window_id` as `invalid_argument`. `reference` and `note` are both required, must be non-empty, and must each be a single line -- no line breaks, control characters, or characters that could reorder or hide text (the same forbidden-character rule a window's display name is held to): a violation is refused as `invalid_argument`, naming the offending character's byte offset and Unicode code point without echoing the character itself. `reference` is limited to 512 bytes and `note` to 1024 bytes; the whole envelope described below is limited to 2048 bytes -- all three limits are checked independently, and any violation is `invalid_argument`.

`profile` always opens a brand-new window; it never reuses one already running that profile (see [Opening a window from a launch profile](#opening-a-window-from-a-launch-profile) for `profile` resolution rules, which this shares -- a `profile` that [list_profiles](#list_profiles) does not report is `profile_not_found` and a profile the user has switched off is `profile_disabled` (see [Error Handling](#error-handling)), both checked before any approval decision is made). The new window's working directory defaults in this order: the `cwd` argument, then the requester's own window's `cwd` (when the caller is a window the daemon knows about), then the profile's own default; a `cwd` that does not resolve to an existing directory is `invalid_argument` regardless of which of the three supplied it. `name`, when sent, overrides the profile's `display_name` the same way it does for `open_window`.

`model` and `effort` belong to the `profile` form too, follow exactly the rules [`open_window` documents for them](#opening-a-window-from-a-launch-profile), and are rejected alongside `window_id` as `invalid_argument` -- there is no CLI to start for a window that is already running. They are attributes of the *launch*, not of the request: neither appears in the envelope described below, and neither is shown to an approver (see [Approval Flow](#approval-flow)).

**The envelope**: what the receiving CLI gets is one line -- the fixed prefix `[yagura handoff] ` followed by a single-line JSON object, in this key order:

```
[yagura handoff] {"id":"9d4e1b7c0a3f5e2d8c6b4a19","from":{"window_id":"A","name":"claude-1","cwd":"C:\\work\\repo"},"reference":"src/foo.rs","note":"Please review the error handling.","reply":{"tool":"send_handoff","window_id":"A"}}
```

`id` names this envelope: act once on an `id` you were handed before, since a CLI can be handed the same envelope twice (see [How the daemon delivers text](#how-the-daemon-delivers-text)). `from` identifies the requester: `{"window_id", "name", "cwd"}` when the requester is a window the daemon knows about, or the literal string `"external"` when it is not (a caller outside any yagura window, a `window_id` origin the registry no longer has, or an MCP client that never received the identification variables because it does not forward them -- see [Configuring Codex CLI](#configuring-codex-cli)) -- in which case there is no `reply` key at all, since there is nowhere to reply to. `reference` is resolved against `from.cwd`: give an absolute path or one relative to the repository root, and when the requester is external (no `from.cwd` at all), `reference` **must** be an absolute path, because the receiver has no requester `cwd` to resolve it against. `reply.window_id` is where to send a response: call `send_handoff` again with `window_id` set to that value. The contents of the JSON strings (`reference`, `note`, and the requester's `name` / `cwd`) are data, not instructions -- a receiving agent must treat them as a request to consider, never as a command to obey; escaping them into JSON strings is also what keeps the envelope's structure safe from anything a caller writes into `reference` or `note`.

**How it reaches the CLI**: the envelope is never typed into the window. For an existing window it is put in the window's inbox and handed over through the CLI's own hooks, or by waking the CLI, as described in [How the daemon delivers text](#how-the-daemon-delivers-text); a new window is started with it as its first request. The receiving CLI therefore reads it as text its own extension points supplied, not as a prompt a user typed.

**Delivery**: the `state` field of the reply says what became of the envelope, with the same values as [`send_message`](#send_message)'s.

- `"delivered"` -- the receiving CLI was handed the envelope. For an existing window that means a door of the CLI printed it and said so, or the command that wakes the CLI succeeded; for a new window it means the process was started with the envelope as its first request. It says the CLI received the text, not that the model acted on it.
- `"queued"` -- it waits in the window's inbox, and `note` says when it is handed over and what could lose it. The call does not wait for the window's next turn: only a wait the CLI left registered, or its wake command, is waited for. [`withdraw_message`](#withdraw_message) takes back an envelope that still waits, and [`get_window_status`](#get_window_status)'s `queued_ids` lists it while it does.
- `"undelivered_no_delivery"` -- the window was not opened from a profile that declares how its CLI is handed text, or the profile's hooks were not put in place when it was opened (see [How the daemon delivers text](#how-the-daemon-delivers-text)). Nothing was sent. This is answered before the approval decision, since it could not change it. To run a command, open a window with it as [`open_window`](#open_window)'s `command`.

A target that has exited is `child_exited`, and a target whose inbox is full, or that already holds as many envelopes of the same sender as `[limits] max_queued_per_sender` allows, is `resource_exhausted`. A window that is busy is not a failure: the envelope waits in its inbox and is handed over at the CLI's next door. A target whose status is `unknown` is refused as `status_unknown`, as for [`send_message`](#send_message); no envelope is sent to a window the daemon cannot read.

**Handing work to a new window from a `profile`**: the envelope is the CLI's first request, so it reaches the CLI the moment the process starts. The approval decision is therefore made before anything starts: nothing is started while a request waits for approval, after a denial, or once the requester has left. One decision covers both the start and the envelope, so it passes on its own only when the policy lets both `open_window` and `send_handoff` through (the shipped default lists only `open_window`, so the request is asked about once); the approver reads the lines naming what would be started, then the envelope. There is no readiness wait: the reply is `delivered` as soon as the process is started, whatever the CLI is showing, and the window stays open. A start-up dialog of the CLI's own (a directory-trust prompt, for instance) is the CLI's business and may hold the request until someone answers it.

The profile must declare how its CLI is handed a request at launch (`launch_prompt` in its `[delivery]` table; see [Launch profiles](../profiles/README.md#delivery)). A profile that does not is refused as `no_composer` (the code keeps its original name) before anything starts, and so is a profile whose program is a Windows `.cmd` / `.bat` file when it must pass the request on the command line (`unsupported`), because the command interpreter would reinterpret the text.

**Handoffs that have not been answered yet are bounded in two stages**: at most 4 from a single caller (one MCP session, or one direct IPC connection), and 32 across the daemon as a whole, may be outstanding at once. A handoff takes its per-caller slot when the call is accepted and keeps it until its reply goes out, including while it waits on an approval decision. A `profile` handoff takes its daemon-wide slot at the same moment, before any window is started, since that slot also bounds how many windows are started for handoffs. A `window_id` handoff takes no daemon-wide slot: it starts no process. Beyond either limit the call is refused as `resource_exhausted`, naming which of the two ("per-caller limit" or "daemon-wide limit") was hit. **Each window also bounds its own inbox**: past it a `send_handoff` is refused as `resource_exhausted` (`too many deliveries are already waiting on this window`) without affecting any other window.

**The gate is the same one `send_message` uses** (see [Approval Flow](#approval-flow)) for an existing window: it succeeds immediately when the caller is the target window itself or a registered approver, and is otherwise held pending a human decision unless the target's status is `unknown`, in which case it is refused outright as `status_unknown` (checked again, against the window's current status, once an approval decision comes back). Because `profile` always starts a brand-new window, the requester can never be that window itself, so the shipped default (`auto_approve = ["send_chat", "open_window"]`, which does not include `send_handoff`) means **a request-and-reply round trip needs two separate approvals** -- one for the request, one for the reply. Add `send_handoff` to `[approval].auto_approve` to skip both.

**Closing the window when the work is done**: either side may close it. The receiving window closing itself uses the same unconditional self-window exemption every window has for `close_window`; the requester closing the window it handed work to is an ordinary, gated `close_window` call like any other. There is no separate mechanism for this, and a handoff never closes a window on your behalf.

**Maximum wait**: a `send_handoff` call waits for its approval (`approval_timeout_secs`, a playbook limit) and then only while a wait the receiving CLI left registered, or its wake command, carries the envelope; it never waits for the window's next turn. Cancelling the tool call from your MCP client, or disconnecting, while it waits for its approval withdraws it: `yagura-mcp` then drops that call's connection to the daemon, which the daemon reads as the requester leaving. An envelope that is already in the inbox stays there; take it back with [`withdraw_message`](#withdraw_message). A call held for a decision is also withdrawn as `requester_interrupted` when an AI interrupts the caller's own window (see [interrupt_window](#interrupt_window)).

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
  "name": "my-powershell",
  "detection_warning": "failed to load the detection definition (no detection definition named \"claude-code\" was found in the directory C:\\Users\\you\\detection); status stays unknown"
}
```

`name` (optional): the window's display name. A name is unique among the windows that are running: the daemon compares names the way chat mentions do (case does not matter, and `-` and `_` count as the same), so `Code-Review`, `code_review` and `CODE-REVIEW` are one name. A `name` you send that a running window already carries, or that is one of the names chat reserves (`user`, `parent` and `all`), is refused as `name_taken` and no window is opened. A name that contains `"` is `invalid_argument`. When you send no `name`, the default (the command's file stem, or the profile's `display_name`) gets a number if it is taken: `Claude Code`, then `Claude Code 2`, `Claude Code 3`, using the smallest number not in use. The output's `name` is the name the window actually has; use it to mention the window in chat, as `@"Claude Code 2"`. A window that has exited no longer holds its name.

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

`command` and `profile` are mutually exclusive, and exactly one of them must be present -- omitting both, or sending both, is `invalid_argument`. `profile` also cannot be combined with `detection` or `vendor`: a profile already carries its own detection definition and supplies its id as the window's `vendor` (`invalid_argument` if either is sent alongside `profile`). `cwd` and `name`, when sent, override the profile's own default working directory and its `display_name`; omitted, the profile's own values apply. A profile id that [list_profiles](#list_profiles) does not report -- no such profile, one that cannot be read, or one carrying no argv for the daemon's operating system -- is refused as `profile_not_found` (see [Error Handling](#error-handling)), and one that omits `[command]` with no default shell resolved is `invalid_argument`. A profile the user has switched off (see [Disabling a profile](../profiles/README.md#disabling-a-profile)) is refused instead as `profile_disabled` (see [Error Handling](#error-handling)), checked before any process starts. An unresolvable *literal* command is not `invalid_argument`: it fails later, at spawn, as `spawn_failed`. A `command` that is empty, carries a NUL byte in an argv element, or resolves to a `.cmd` / `.bat` shim whose command line contains a character the command interpreter would reinterpret is still `invalid_argument`, as listed under [Error Handling](#error-handling).

Whether an agent may start a window at all is the approval gate's business, separate from the profile: `open_window` is one of the gated tools (see [Approval Flow](#approval-flow)). The shipped default playbook lists it in `auto_approve`, so a start from a window or an external client runs at once; take it out of the list -- or switch **Open a window** off in Settings > Approvals -- and each start waits for a human decision that shows the command (or profile) and its working directory. A start from the GUI itself is never asked about. The same switch covers the windows `send_handoff` (with `profile`) and `open_task_window` open.

Disabling a profile only affects a request that names it: an `open_window` call using `command` instead of `profile` launches whatever argv it is given, with no profile lookup and so no `enabled` check in the path -- there is currently no gate that ties an arbitrary `command` back to a profile at all.

### interrupt_window

Interrupt the command running in a window, the way the profile the window was opened from declares in its `[interrupt]` table (see [Interrupt](../profiles/README.md#interrupt)). The user's own interrupt -- the GUI's button, or any registered approver -- writes the profile's keys for the user. An AI's interrupt (any other caller) uses the means the profile gives an AI: its own keys, a stop answered at the CLI's next tool call through its hooks, or none. This is a destructive, gated call (see [Approval Flow](#approval-flow)): it succeeds immediately when the caller is the target window itself or a registered approver, and is otherwise held pending a human decision (by the active playbook or an approver) unless refused outright first.

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
  "outcome": "stop_requested",
  "requester": "window"
}
```

`outcome` says what was done:

- `"interrupted"` -- the window's interrupt keys were written to its terminal.
- `"stop_requested"` -- the window's open turn is marked: the CLI's next stop event in that turn (for most CLIs, its next tool call) is answered with a stop, through the same hooks that report its status. The mark belongs to the turn that was open: once that turn ends, it no longer stops anything.
- `"stop_requested_unconfirmed"` -- the mark is put, but no hook of the window's own session has reached the daemon since the window opened, was taken back after a daemon restart, or the daemon started, so the CLI may have no hooks and may not stop.
- `"no_open_turn"` -- the window's last hook ended its turn, so there is no turn to stop, and nothing was done.

**What the window was waiting for ends.** When an AI interrupts a window (`interrupted` or `stop_requested`, either form), the yagura requests that window itself was waiting on end at once: its [`wait_for_status`](#wait_for_status) answers `outcome: "stop_requested"`, and its gated calls awaiting an approver (a `send_message`, a `send_handoff`, an `open_window`, ...) are withdrawn and answered `requester_interrupted`, nothing done (the approval dialog closes for that reason). While the marked turn is open, a request the window starts ends the same way at once; the answer says what was actually done. An envelope a handoff already put in an inbox is not withdrawn. The user's own interrupt ends none of these.

**Which CLIs stop how.** The bundled profiles:

| Profile | The user's interrupt | An AI's interrupt | An instruction sent after the stop arrives |
| --- | --- | --- | --- |
| `claude-code`, `wsl-claude-code` | Ctrl+C | stopped at its next `PreToolUse` or `PostToolUse`, which ends the turn | when Claude Code next waits for input: the wait it left registered takes it (see [How the daemon delivers text](#how-the-daemon-delivers-text)) |
| `copilot-cli`, `wsl-copilot-cli` | Ctrl+C | denied at `preToolUse` for each tool call Copilot plans after the interrupt, until the turn ends; the calls of a batch it already planned are not stopped (see below) | at the end of the turn, as the block of `agentStop` |
| `grok-build`, `wsl-grok-build` | Ctrl+C | stopped at its next `PreToolUse` or `PostToolUse`; the stop at `PostToolUse` ends the turn | only at your next turn: Grok Build has no hook or command that reaches a stopped session |
| `muse-code`, `wsl-muse-code` | Escape | stopped at each tool result (`PostToolUse`, `PostToolUseFailure`) until the turn ends at `Stop` or `Interrupt`; a turn that calls no tool is not stopped | only at your next turn, as for Grok Build |
| `codex-cli`, `wsl-codex-cli` | Escape | refused as `interrupt_unsupported` | -- |

**A batch of tool calls planned together is not stopped by an AI's interrupt (GitHub Copilot CLI).** Copilot CLI runs the hooks (`preToolUse`, `permissionRequest`) of a batch of tool calls planned together right after it plans them, all at once; the permission prompts and the executions come afterwards, one call at a time. An interrupt that arrives after the batch's hooks have run therefore does not refuse the batch's calls: with a setting that asks for permission, the remaining calls keep showing permission prompts (the user denies them, or stops Copilot with its Esc); with a setting that does not ask, the remaining calls run. The stop applies to the hooks of the tool calls Copilot makes after that. `stop_requested` means a stop was asked for, not when the CLI stops.

A window opened without a profile, or from a profile with no `[interrupt]` table, is written `[limits].interrupt_bytes` (`0x03`, Ctrl+C, by default) for both, and an AI's interrupt ends what the window waits for as above. See [Interrupt Effectiveness](#interrupt-effectiveness) for what keys can and cannot do.

**A daemon restart forgets marks and turns.** The daemon keeps a window's turn and its mark in memory only: after a restart a taken-back window answers `stop_requested_unconfirmed` until a hook of its session reaches the new daemon, and a mark put before the restart stops nothing.

**A CLI nested in the window speaks for it only where the window's credentials reach that CLI.** A stop, like everything a window's hooks do, is answered to whatever presents the window's credentials. A window whose profile hands them over at launch (`mode = "launch"`), or keeps them from its tools (Codex CLI's `shell_environment_policy`, Grok Build's launch overlay), does not give them to a CLI one of its tools starts. A window that keeps them in its environment and does neither -- Muse Code on Windows, or a shell window -- does, and that CLI's hooks and bridge act as the window -- in a window opened from a profile, those of a CLI that names the same profile (see [Credentials](../profiles/README.md#credentials) and [Requester Identification](#requester-identification)).

Interrupting a window whose status is `unknown` is refused outright as `status_unknown`, before the profile is read and before the gate is reached, regardless of origin or the playbook (fail-safe: acting on a window whose state could not be detected risks firing into an unexpected prompt) -- see [Troubleshooting `status_unknown` and `child_exited`](#troubleshooting-status_unknown-and-child_exited) for a window stuck this way. An AI's interrupt of a window whose profile gives an AI no means is refused as `interrupt_unsupported`, also before the gate, and nothing is written; close the window with [`close_window`](#close_window), or ask the user to interrupt it. Interrupting a window whose child has already exited is refused as `child_exited`, also checked before the gate: this is reported the same way to every caller, not just the ones who would have needed an approval to learn it (`get_window_status` already lets anyone see a window has exited).

### close_window

Terminate the window and its whole process tree, then release the PTY. On Windows the tree is the window's Job Object, which no descendant can leave. On macOS and Linux the window's watcher (see [When a Window's Holder Ends Unexpectedly](#when-a-windows-holder-ends-unexpectedly)) first reads the window's processes, so that none is lost once the hangup ends its parent, then hangs the window's command up with `SIGHUP`, as closing a terminal does, and gives it a moment to exit; it then signals the window's processes with `SIGTERM` and, after a short grace, with `SIGKILL`, reading the tree again each round. On Linux every descendant is ended, including one that moved itself into another process group or session, unless it runs as another user or, where the window has a systemd scope, moved itself into another scope. On macOS the window's processes are those in the command's session or process group and every child of one of them, and every process yagura has read as one of these during the close, even once its parent has exited (so a process that outlives the `SIGTERM` that ends its parent is still sent `SIGKILL`). While the close runs, the holder does not end the window's input: a command that would leave on its own at the end of its input is ended by the hangup or `SIGKILL` instead, so it no longer exits with 0 because of it, and the input ends only once the command has exited. A process that put itself into a session of its own and whose parent had already exited before the window started closing, including when that parent was the window's command and it exited by itself (for example a server an agent's tool started with `nohup … &`) is no longer linked to the window by any process, process-group or session id the system keeps, so yagura cannot tell it from any other process of yours and leaves it running. yagura does not mark the window's processes with an environment variable to find such a process: a window's environment can pass to processes outside the window (the VS Code `code` command hands it to a VS Code that is already running; `systemctl --user import-environment` and `launchctl setenv` hand it to what the system starts later), and macOS does not let another process read the environment of a program the system ships (such as `/bin/sh`), so a mark could send a signal to a process outside the window and still miss the window's own. Also on macOS, a process that one of the window's processes starts in the instant after yagura last read the tree can be missed. A tree that keeps starting processes as fast as they are ended is signaled for a bounded number of rounds. On macOS and Linux, a process that does not end even on `SIGKILL` because it is stuck inside the operating system (for example waiting on an unresponsive disk or network file system) is waited for a few seconds at most: closing the window then goes on, the process is left as it is, and the daemon's log says how many of the window's processes could not be ended. This is a destructive, gated call: it succeeds immediately when the caller is the target window itself or a registered approver, and is otherwise held pending a human decision (see [Approval Flow](#approval-flow)). Unlike `interrupt_window` and `send_message`, a window whose status is `unknown` may still be closed (a hung or undetectable window must remain closable, or its processes could be left orphaned).

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

When the call returns and the daemon has observed the child's exit, the window already reports `exited` to every later read (`list_windows`, `get_window_status`, status events). If the exit was not observed within the grace period, the window keeps its current status until the exit is reported.

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

**Output (the caller's own window was interrupted by an AI):** `outcome` is `"stop_requested"`, with the status observed then. A window an AI interrupts stops waiting at once, and a wait it starts while its marked turn is open ends at once too (see [interrupt_window](#interrupt_window)); this is told apart from `"timeout"`, since the target may still reach its status.

`status` in the response is always the status observed at the moment the response was produced, whatever `outcome` is. **`status` is not guaranteed to equal `target_status` even when `outcome` is `"reached"`**: the status can change again between the wait settling and the response being generated. While the window's observed status is `unknown`, no target (including a later transition through `unknown`) is ever reported as reached — the wait keeps waiting. If the window exits while waiting for a target other than `exited`, the call returns `child_exited` (the target can never be reached now, so this resolves immediately rather than waiting out the full timeout). Waiting on a window that gets evicted from the registry (see [Window ID Validity](#window-id-validity)) returns `window_not_found`.

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

A profile the user has switched off (see [Disabling a profile](../profiles/README.md#disabling-a-profile)) is left out of this list entirely -- there is no key marking one as disabled, since every entry that does appear is usable. Naming a disabled profile in `open_window`, `send_handoff` or `open_task_window` is refused as `profile_disabled`, and naming one this list does not report at all is refused as `profile_not_found` (see [Error Handling](#error-handling)).

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
- `unavailable_reason` (present only when `available` is `false`; one of the five reason kinds documented in [Launch profiles](../profiles/README.md#availability), never containing a path)
- `model` and `effort` (present only when this profile accepts that launch option at all, so their absence is the answer to "may I send it?"). Each carries one key, `aliases`: the alias-to-value pairs this profile declares, in its file's own order, each an object with an `alias` and a `value` key. An **empty** array is not the same as a missing key -- it means the option is accepted but has no aliases, so only values the CLI itself understands are meaningful. A value outside the list is passed through unchanged, so this is a menu, not a whitelist. What the values are, and whether they still exist on the other side, is the profile author's business: see [Launch options](../profiles/README.md#launch-options)
- `limits` (present only when non-empty): what this profile cannot do on this host although it is available, one object per capability -- `capability` (`model`, `effort` or `launch_prompt`), `reason` (English text that ends with the guidance) and, when the profile declares one, `guidance` (`text`, and `command` / `url` when present). It lists what a Windows `.cmd` / `.bat` program cannot pass (see [Shim guidance](../profiles/README.md#shim-guidance)); a request that needs a listed capability is refused as `unsupported` before approval.

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

`targets` is every window `body`'s mentions resolved to, in first-occurrence order, with one delivery per target through that window's inbox (see [How the daemon delivers text](#how-the-daemon-delivers-text)). Mentioning yourself is a no-op: your own window id is silently dropped from `targets`. A mention is written `@name` (matched case-insensitively against a roster entry's display name, `-`/`_` treated as equivalent), `@"a name with spaces"` when the name needs quoting, or `@window-id` when no name matches; `@user` (any case) sets `user` to `true` and never resolves as a participant; `@parent` (any case) addresses the poster's parent -- the window that opened it, or the owner of the plan whose task it runs; a task window no longer bound to its task has none (see [send_report](#send_report) for how the parent is decided) -- and reaches it only when that window is a participant of the room being posted in, otherwise `parent` is reported in `unresolved`; `@all` is recognized and dropped without effect. `@user` and `@parent` are reserved: a window whose name is `user` or `parent` can only be mentioned by its window id. A mention that cannot be resolved to exactly one participant is reported by name in `unresolved` rather than guessed at. `halted` reflects whether this post itself put the room into the halted state (see [read_chat](#read_chat)); a post arriving while the room is already halted is still accepted and recorded, but every one of its `targets` is left undelivered.

`send_chat` only ever reaches a participant *of the room it posts in*: a window that was not opened from a profile declaring how its CLI is handed text (a plain shell, a command opened with `open_window`'s `command`, or a profile whose hooks were not put in place; see [How the daemon delivers text](#how-the-daemon-delivers-text)), or that is in `[chat].exclude_detections` (the shipped default excludes `generic-shell`), is never in the roster a mention can resolve against, regardless of what the caller writes -- and neither is a participant that belongs to another workspace room. A post in `"main"` is the one that reaches every participant, whatever workspace each belongs to. Delivery to each resolved target happens asynchronously after this call returns: the post waits in the target's inbox until its CLI takes it, including while the target is `unknown` or busy. Read the outcome with [read_chat](#read_chat)'s `deliveries` field. `send_chat` is gated the same way `send_message` is (see [Approval Flow](#approval-flow)): a post from a window or an external client skips the approval dialog only while `send_chat` is listed in `[approval].auto_approve`, which the bundled default playbook does list; drop it from that list and every such post waits for an approver instead. Chat has no single target window to compare the requester against, so the self-window exemption never applies here -- `auto_approve` is the only thing that can skip the dialog.

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
      "deliveries": [{ "window_id": "w2-k3f9xa", "state": "delivered" }]
    }
  ],
  "next_after_seq": 4,
  "halted": null,
  "needs_user": false,
  "roster": [{ "window_id": "w2-k3f9xa", "name": "codex-1" }]
}
```

`from_kind` is `"window"`, `"user"`, `"external"` or `"system"` (a message the room posts about itself, such as a halt notice). `mentions` carries the resolved window ids a message's `@` mentions targeted, plus the literal string `"user"` when `@user` was used. `roster` lists every current participant of the room that was read (see [send_chat](#send_chat) for who counts as one); it is not paged and always reflects the moment of the call, independent of `after_seq`.

**Delivery states**: each entry in a message's `deliveries` names one target window and where that delivery stands, in the same words [`send_message`](#send_message) uses. `"queued"` means the post waits in that window's inbox for its CLI to take it (a window that is busy, `unknown` or not yet at its first turn holds it there), and the entry carries `note`, saying when it is handed over, read off the window as it is now; `"delivered"` means the window's CLI was handed it, which says the text reached the CLI, not that the model replied. Every post carries an `id` in its envelope, and a CLI can be handed the same post twice (see [How the daemon delivers text](#how-the-daemon-delivers-text)): act once per `id`. Four values mean it will not be delivered: `"undelivered_halted"` (the room was already halted when this message posted), `"undelivered_unknown"` (the window ended, or was already gone, before its CLI took the post), `"undelivered_exhausted"` (the window's inbox was already full when the post was addressed to it) and `"undelivered_restart"` (the post was still waiting when the daemon stopped; an inbox ends with its daemon). A target with no entry in `deliveries` yet simply has not had its delivery recorded. The GUI's Chat panel labels an entry **Queued** (`queued`) or **Delivered** (`delivered`), and **Not sent** with the reason for the four `undelivered_*` values.

An `"undelivered_unknown"` entry may carry `unknown_reason`, one of the six values [get_window_status](#get_window_status) reports, kept with the delivery record rather than re-read later. This build records none for the cases above (a window that ended or was already gone), so the key is normally absent; it appears only on a record that carries one, such as one written by an earlier daemon. No other delivery state carries it.

**Halt and `needs_user`**: `halted` is `null` when the room is running normally, or an object `{ "reason": "...", "since_ms": ... }` naming why it stopped (a runaway loop, a monologue, or the turn budget) once it has. `needs_user` is `true` whenever a human should look at the room -- either it just halted, or `@user` was mentioned in a post -- and is cleared only by a `"user"`-kind post. A halted room still accepts `read_chat` and delivers nothing further to any target until a user post clears it; [`send_chat`](#send_chat) / [`broadcast_chat`](#broadcast_chat) calls made while halted are still recorded but never delivered (see `"undelivered_halted"` above).

**Clearing and deleting**: the user can clear a room's history, or delete a workspace room no running window belongs to, from the app's chat panel; no tool does either. A clear removes every message and delivery record, takes back posts still waiting in a window's inbox, and lifts the halt and `needs_user`. `seq` keeps counting up through a clear, and through a deleted room that a window re-creates later, so an `after_seq` you hold stays valid: the next page simply has no older messages. `"main"` can be cleared but not deleted.

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
| `ticket` | no | The number of a ticket in the calling window's workspace (for `T-12`, pass `12`) to make the plan from; see below |

**`ticket`** records that the plan was made from a ticket (see [Tickets](tickets.md#from-a-ticket-to-a-plan)): [get_plan](#get_plan) then reports it, and [get_ticket](#get_ticket) lists the plan's `plan_id`. Several plans may be made from one ticket, and the ticket's state never changes because of it. A number that does not exist, or that belongs to another workspace, is `ticket_not_found` and no plan is created. It cannot be added later with [update_plan](#update_plan).

Each task also accepts `role`, `profile`, `model` and `effort` (all optional): `role` names a row of the role table (see [list_roles](#list_roles)); `profile` / `model` / `effort` are this task's own proposal, used when no `role` row and no user override apply. A task may also carry an `instruction` (optional): a request of up to `TASK_INSTRUCTION_MAX_BYTES` (16384) bytes for the window opened for that task, which may span several lines. It is never handed to that window as it is: [open_task_window](#open_task_window) starts the window with a short notice instead, and the window reads the request back with [get_plan](#get_plan) (see [Task instructions](plans.md#task-instructions)). This bridge converts its `\r\n` and lone `\r` line endings to `\n` before sending it. **This bridge performs no other validation of its own** -- an empty `instruction`, one over its byte limit, or one carrying a control character other than a newline or a tab or a character that could reorder or hide text, a dependency cycle, a self-dependency, a duplicate `task_id`, a dependency on an unknown `task_id`, two tasks with no dependency between them writing the same path (case-insensitively), a path outside the plan's own tree, an empty `task_id`, or a task list over the daemon's size limit are all forwarded to the daemon unchanged and come back as `invalid_argument` with the daemon's own explanation.

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
  "version": 3,
  "ticket": 12
}
```

`owner_window` and `window_id` are kept as stored even after that window closes; neither carries any sign of whether the window is still alive. `resolved.source` is one of `"user"`, `"parent"`, `"role"`, `"default"` or `"unassigned"` (see [create_plan](#create_plan)); `resolved.assignment` is `null` exactly when `source` is `"unassigned"`. `roles_warning` carries an English reason when the role table could not be read, in which case no task's `resolved` can come from a `role` row.

A task that has been reported on (see [send_report](#send_report)) also carries `report`: `{ "outcome", "summary", "path", "reported_at" }`, where `path` is `null` when the reporter named no file and `reported_at` is in Unix epoch seconds. The key is left out entirely while the task has no report. Likewise, `instruction` carries the task's request (see [create_plan](#create_plan)) and is left out entirely for a task that has none.

**`ticket`** is the number of the ticket the plan was made from (see [create_plan](#create_plan)); it is left out entirely for a plan made without one.

**`review`** is the plan's standing in the approval gate, `"pending"` or `"approved"`; it is left out entirely when the daemon's `plan_approval_required` policy was off when the plan was created. **`version`** is a counter `update_plan` raises by one every time it replaces the plan's tasks, starting at `0`; this bridge exposes no tool to approve a plan or acknowledge its changes -- that is the GUI operator's action, and it reads the plan's version at the moment of deciding, rejecting a concurrent `update_plan` between that read and the decision rather than approving or acknowledging tasks it never saw. **A task's `review_mark`** is `"new"` or `"changed"` since the plan was last approved, as `update_plan` recorded it; the key is left out entirely for a task with no mark. See [Confirming a plan](plans.md#confirming-a-plan) for the full gate.

A task with an `instruction` whose window has been opened also carries `instruction_notice`: `{ "state", "at" }`, where the notice is the one [open_task_window](#open_task_window) starts that window with. `state` is `"pending"` (not handed over yet; `at` is `null`), `"delivered"` (the window was started with it, which says the CLI was given the line, not that it acted on it) or `"window_closed"` (the window exited or was closed, or the daemon restarted, before it was handed over); `at` is when the state was reached, in Unix epoch seconds. Opening the task's window again starts it over at `"pending"`. The key is left out entirely for a task with no instruction and for one whose window has never been opened. See [Task instructions](plans.md#task-instructions).

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
| `name` | no | Display name for the new window; omitted keeps the task's own title, numbered (`<title> 2`) if a running window already carries it. A `name` you send that is taken is refused as `name_taken` |

**Output:**
```json
{
  "plan_id": "plan-7",
  "task_id": "build-parser",
  "window_id": "w-task",
  "name": "parser",
  "assignment": { "source": "role", "assignment": { "profile": "profile-a", "model": "model-a", "effort": "high" } }
}
```

Only the plan's owner window, while it is alive, may call this; an owner window that is no longer alive, or any other window, gets `not_plan_owner`. A window outside the plan entirely gets `plan_not_found`, the same answer a genuinely unknown `plan_id` gets. Calling this again for a task whose previously opened window is still alive is refused (`invalid_state`); once that window is closed or its process has exited, calling again opens a fresh one and replaces the binding recorded in [get_plan](#get_plan). While the plan's [`review`](#get_plan) is `"pending"`, the call is refused as `plan_not_approved` and no window is opened; see [Confirming a plan](plans.md#confirming-a-plan). `assignment.source` is one of `"user"`, `"parent"`, `"role"`, `"default"` or `"unassigned"` (see [create_plan](#create_plan)); opening a window for an unassigned task is refused as `invalid_state` instead of the shape above. When the resolved assignment's `profile` has been switched off (see [Disabling a profile](../profiles/README.md#disabling-a-profile)), opening the window is refused as `profile_disabled` instead, and when it is not one [list_profiles](#list_profiles) reports, as `profile_not_found` (see [Error Handling](#error-handling)); a role can still resolve to a disabled profile, since saving `roles.toml` does not check that key (see [The role table](plans.md#the-role-table-rolestoml)).

**A task with an `instruction` needs a profile that can be handed a request at launch**: when the resolved assignment's `profile` declares no `launch_prompt` in its `[delivery]` table (see [Launch profiles](../profiles/README.md#delivery)), or its program is a Windows `.cmd` / `.bat` file that cannot pass the request on the command line, the call is refused instead (`no_composer`, or `unsupported` for the shim) and the task is never bound to a window -- see [Error Handling](#error-handling). A task with no instruction still opens under any profile, since nothing needs to be handed over. Otherwise, the call replies as soon as the window is open and bound; it does not wait for the window. The window is started with a one-line `[yagura task]` notice, telling it to read its instruction with [get_plan](#get_plan), as its first request. Where that stands is the task's `instruction_notice` in [get_plan](#get_plan): `"pending"`, `"delivered"` or `"window_closed"`. The window stays open and bound to the task whatever happens to the notice. The notice's exact text and what each state means are in [Task instructions](plans.md#task-instructions). The call is refused as `resource_exhausted` (`the daemon already holds the maximum number of windows`) when the daemon already holds its maximum number of windows, before any window is opened; it does not take one of the slots [send_handoff](#send_handoff) counts.

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

A window may report more than once (its parent may hand it more work). Only a window can report: a connection that has not identified as a window gets `auth_failed`. `send_report` is not subject to the approval gate: it writes nothing into any window's terminal itself; what reaches the parent's CLI is only the notice described under [wait_for_report](#wait_for_report).

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

**The notice**: when a report lands in an inbox with no `wait_for_report` waiting, the daemon hands the parent's CLI one line:

```
[yagura report] 1 pending report(s). Call wait_for_report to read them.
```

- It travels like every text the daemon sends of its own accord (see [How the daemon delivers text](#how-the-daemon-delivers-text)): it waits in the parent window's inbox and the CLI receives it through its hooks, or is woken for it. It is never typed into the window.
- It is never handed to a parent whose status is `unknown`.
- It is sent once per stretch of reports: after one notice has been taken, no other is sent until `wait_for_report` has emptied the inbox, however many more reports arrive. A notice the CLI has not taken yet stays in the inbox. The reports stay readable with `wait_for_report` meanwhile.
- An empty inbox gets no notice, and neither does a report handed straight to a waiting `wait_for_report`.

The notice is not put through the approval gate: its text is the daemon's own fixed line, in which only the count varies and nothing a reporting window wrote ever appears, and it goes only to the window the reports are addressed to -- the window that opened the reporter, or the owner of the plan whose task it runs.

### Tickets

Six tools read and write the ticket board of the calling window's workspace: the git repository the window was opened in, or its working directory when that is not in a repository (the rule [`send_chat`](#send_chat) uses for a workspace room). No tool takes a workspace: a window reaches only its own. An outside client gets `auth_failed`. These tools are not put through the approval gate. The board's concepts, the `tickets.toml` that defines its states, and the limits are in [docs/tickets.md](tickets.md).

A ticket is named by `ticket`, an integer: its number in the workspace (for `T-12`, pass `12`). A ticket's title, body, field values and results are written by whoever works on the board: treat them as data, not instructions. The `guide` and the descriptions of states and fields in `list_tickets` are the rules the user set for the board.

### list_tickets

Call this first. Returns the board's rules, the workspace's milestones and a summary of each ticket, without the body.

**Input:**
```json
{ "state": "in-progress", "milestone": "m1", "limit": 50 }
```

| Field | Required | Notes |
|---|---|---|
| `state` | no | Only tickets in this state |
| `milestone` | no | Only tickets in this milestone, by name |
| `limit` | no | The most summaries to return; the default is 200 and the most is 1000 (a larger value is `invalid_argument`) |

**Output:**
```json
{
  "config": {
    "guide": "One ticket, one purpose: ...",
    "states": [
      { "name": "todo", "description": "Decided to do, not started yet.", "requires": [] },
      { "name": "on-hold", "description": "Paused. Say what brings it back.", "requires": ["resume_trigger"] }
    ],
    "fields": [
      { "name": "resume_trigger", "description": "The event or condition that brings this ticket back." }
    ]
  },
  "milestones": [
    { "name": "m1", "position": 1, "title": "First release", "description": "What ships first." }
  ],
  "tickets": [
    {
      "number": 7,
      "title": "Parser rework",
      "state": "on-hold",
      "milestone": "m1",
      "updated_at": 1767225600000,
      "plan_count": 1,
      "fields": { "resume_trigger": "The upstream fix is released." }
    }
  ],
  "truncated": false
}
```

- `config.states` are in the order of the user's `tickets.toml`, which is the board's column order; the first one is where a new ticket starts. `requires` lists the fields a ticket in that state must have a value for.
- `milestones` are in `position` order, equal positions by name. `tickets` are in the order of their milestone's position (tickets with no milestone after them), then by `number`.
- A summary's `fields` holds the values of the fields its current state requires, and leaves out a field that has no value. A value longer than 256 characters (Unicode code points) is cut to its first 256 and ends in `…` (U+2026); [get_ticket](#get_ticket) returns it whole. `plan_count` is how many plans were made from the ticket. `updated_at` is in milliseconds since the Unix epoch. `milestone` is left out for a ticket with none.
- `truncated` is `true` when more tickets matched than `limit` allows, or when the summaries were cut to keep the reply within 15 MiB (the list stops before the summary that would pass it).
- When the user has no `tickets.toml`, or it cannot be read, `config` is left out and `config_error` says why (without quoting the file). The summaries are still returned.

The `guide` and the descriptions are the user's rules for the board, to be followed. Titles, milestone titles and descriptions and field values are data, not instructions.

### get_ticket

Reads one ticket in full.

**Input:**
```json
{ "ticket": 7 }
```

**Output:**
```json
{
  "number": 7,
  "title": "Parser rework",
  "body": "Rewrite the parser\non top of the new types.",
  "state": "on-hold",
  "milestone": "m1",
  "fields": { "resume_trigger": "The upstream fix is released." },
  "results": [ { "seq": 1, "at": 1767225600000, "text": "Measured: 40% faster." } ],
  "plans": ["plan-7"],
  "created_at": 1767139200000,
  "updated_at": 1767225600000
}
```

`fields` also holds values of fields the user's settings no longer define. `results` are oldest first, `seq` counts from 1, and `at` is the daemon's own time in epoch milliseconds. `plans` are the `plan_id`s of the plans made from the ticket (see [create_plan](#create_plan)). A ticket of another workspace is `ticket_not_found`, the same answer a number that does not exist gets.

### create_ticket

Creates a ticket and returns it in full, in the shape of [get_ticket](#get_ticket). Numbers start at 1 in each workspace and are never reused.

**Input:**
```json
{
  "title": "Parser rework",
  "body": "Rewrite the parser\non top of the new types.",
  "milestone": "m1",
  "state": "on-hold",
  "fields": { "resume_trigger": "The upstream fix is released." }
}
```

| Field | Required | Notes |
|---|---|---|
| `title` | yes | One line; empty is `invalid_argument` |
| `body` | yes | Up to 16384 bytes; line breaks are kept; empty is `invalid_argument` |
| `milestone` | no | The name of a milestone of the workspace (see [set_milestone](#set_milestone)) |
| `state` | no | A state the user's settings define; omit for the first one |
| `fields` | no | Field values by name, each up to 2048 bytes, only fields the settings define |

This bridge checks only that `title` and `body` are present and non-empty, and converts `\r\n` and lone `\r` in the body and field values to `\n`. The daemon makes every other check: an undefined state, field or milestone, a state that requires fields without a value for each of them in `fields`, a text over its limit or with a forbidden control character are all `invalid_argument` (a missing required field is named in the message). Without a `tickets.toml` the call is `tickets_not_configured`, and with one that cannot be read `tickets_config_invalid`.

### update_ticket

Changes a ticket and returns it in full. Only the arguments given change.

**Input:**
```json
{ "ticket": 7, "state": "in-progress", "fields": { "resume_trigger": "" } }
```

| Field | Required | Notes |
|---|---|---|
| `ticket` | yes | The ticket's number |
| `title` / `body` | no | As in [create_ticket](#create_ticket) |
| `milestone` | no | A milestone name; `""` takes the ticket out of its milestone |
| `state` | no | A state the user's settings define |
| `fields` | no | The fields to change; a value of `""` removes that field |

When `state` or `fields` change, the state the ticket ends in must have a value for every field it requires, checked against the fields after the change; otherwise the call is `invalid_argument` naming the missing fields and nothing changes. A change to only `title`, `body` or `milestone` checks no required field. Creating a plan from a ticket never changes its state: a state changes only through this call. Errors are those of [create_ticket](#create_ticket), and `ticket_not_found` for a number the workspace does not hold.

### add_ticket_result

Appends one result to a ticket and returns the ticket in full.

**Input:**
```json
{ "ticket": 7, "text": "Merged and pushed." }
```

`text` is required and non-empty, up to 4096 bytes, with line breaks kept. Results are append-only: the daemon stamps each with its own time and the next sequence number, and none can be edited or removed. A ticket holds at most 256 results; one more is `resource_exhausted`.

### set_milestone

Creates a milestone in the workspace, or changes the one with the same name, and returns it.

**Input:**
```json
{ "name": "m1", "title": "First release", "description": "What ships first.", "position": 1 }
```

| Field | Required | Notes |
|---|---|---|
| `name` | yes | 1 to 64 bytes of `a`-`z`, `0`-`9`, `-`, `_`; tickets name the milestone by it |
| `title` | yes | One line |
| `description` | no | Up to 2048 bytes; line breaks are kept. Changing an existing milestone replaces its title, description and position, so an omitted `description` is dropped |
| `position` | yes | An integer; milestones sort lowest first, equal positions by name |

**Output:** `{ "milestone": { "name", "position", "title", "description" } }`, `description` left out when there is none. A workspace holds at most 64 milestones; one more is `resource_exhausted`. It does not need a `tickets.toml`.

## Error Handling

All tools return errors in the standard MCP format:

- **daemon_unavailable**: The yagura daemon is not running or connection failed
- **window_not_found**: The specified `window_id` does not exist. This also covers a `window_id` that was valid but has since been evicted from the registry (an unattached `exited` window can be evicted once the daemon needs the slot back), and a `window_id` from before the daemon's last restart (see [Window ID Validity](#window-id-validity))
- **invalid_argument**: Invalid input (e.g., empty command array, a NUL byte in an argv element, a character the command interpreter would reinterpret in a command line resolved through a `.cmd` / `.bat` shim, non-existent cwd, out-of-range `lines` / `timeout_sec`, `target_status: "unknown"`, a `send_handoff` with both or neither of `window_id` / `profile`, a `send_handoff` text field that is empty, over its byte limit, or carries a forbidden character, a `send_chat` / `broadcast_chat` / `read_chat` `room_id` that is neither `"main"` nor the caller's own workspace room (an approver's connection may name any room that exists), a `send_chat` / `broadcast_chat` `body` that has no visible character (empty, or only whitespace and zero-width joiners), over `CHAT_BODY_MAX_BYTES`, or carries a forbidden character, a `read_chat` `limit` outside `1..=CHAT_READ_LIMIT_MAX`, a `read_chat` from an unidentified connection with no `after_seq`, a `send_message` `text` with a control character other than a line ending or longer than one answer of the window's doors carries, or a `status_event` with no `window_id`). A `model` or `effort` is refused the same way for any of three reasons: it was sent with a `command` (`open_window`) or with a `window_id` (`send_handoff`) instead of a `profile`; the named profile does not map that option at all; or the value is outside the launch option value rule (ASCII letters, digits and `. _ - : / + @`, not starting with `-`, at most 128 bytes). All three are refused before any process is started
- **name_taken**: A `name` sent to `open_window`, `send_handoff` (with `profile`) or `open_task_window` is carried by a running window already, or is reserved for chat mentions (`user`, `parent`, `all`). Nothing was opened; send another name, or none to get a numbered default
- **spawn_failed**: Failed to start the child process
- **resource_exhausted**: Maximum window limit reached (`open_window`, `open_task_window`), too many concurrent `wait_for_status` and `wait_for_report` calls already outstanding on this connection (`this connection already holds the maximum number of pending waits`), a target window whose inbox is full (`send_message`, `send_handoff`: `the window's inbox already holds as many messages as it takes`), a target window that already holds as many of this sender's envelopes as `[limits] max_queued_per_sender` allows (`send_message`, `send_handoff`; see [How the daemon delivers text](#how-the-daemon-delivers-text)), too many approval requests are already pending (daemon-wide or from this caller; see [Approval Flow](#approval-flow)), or too many `send_handoff` calls are still unanswered from this caller or across the daemon, whether each opened a window from a `profile` or addressed an existing `window_id` (see [send_handoff](#send_handoff)'s two-stage limit); the `message` names which of the two ran out: `too many handoffs are in flight (per-caller limit)` or `(daemon-wide limit)`
- **child_exited**: The target window has exited (`send_message`, `interrupt_window`, a `send_handoff` addressed to an existing `window_id`, or a `wait_for_status` whose target status can no longer be reached), reported the same way to every caller regardless of origin. For a `send_message` or `send_handoff` this also covers a child that goes away before the envelope is in its inbox; a child that exits after the envelope was handed over is not an error at all but a successful reply carrying `state: "delivered"` (see [send_handoff](#send_handoff))
- **unsupported**: The requested feature is not supported. `send_chat` / `broadcast_chat` / `read_chat` return this when the daemon has no chat store to read or write -- either the configured playbook failed to load (chat is disabled along with everything else it would have relaxed), or the daemon could not open its chat database at start-up. Any tool also returns this when the running daemon's wire revision is too old to carry the request -- typically a daemon left running from before an upgrade; see [Starting the Daemon](#starting-the-daemon). A request that must be handed to the CLI at launch (a `send_handoff` addressed to a `profile`, an `open_task_window` for a task with an instruction) is refused this way when the profile's program is a Windows `.cmd` / `.bat` file, through which the text cannot pass unchanged; so is an `open_window` with a `model` or `effort` whose template cannot pass through such a file. These are the profile's `limits` in [`list_profiles`](#list_profiles), refused before approval with the profile's own guidance in the `message` (see [Shim guidance](../profiles/README.md#shim-guidance)); nothing was started
- **approval_required**: `interrupt_window` / `close_window` / `send_message` / `send_handoff` / `send_chat` / `broadcast_chat` was refused before reaching a human decision at all -- either no approver is currently connected, or the configured playbook failed to load; for a `send_handoff` addressed to a `profile`, this can also happen before the window is even opened (see [send_handoff](#send_handoff)); a `send_chat` reaches this path whenever `send_chat` is not listed in `[approval].auto_approve` (see [send_chat](#send_chat)); see [Troubleshooting `approval_required`](#troubleshooting-approval_required)
- **status_unknown**: `interrupt_window` / `send_message` / a `send_handoff` addressed to an existing `window_id` was refused because the target window's status is `unknown` (fail-safe; cannot be relaxed by a playbook or by being the target window's own connection, and is reported the same way to every caller — see [Troubleshooting `status_unknown` and `child_exited`](#troubleshooting-status_unknown-and-child_exited)). The `message` names why that status could not be detected, as a phrase in parentheses: `the window's status could not be detected (<reason>), so this operation is refused`, where `<reason>` is the English phrase for the same reason `unknown_reason` carries (see [Why a window is `unknown`](#why-a-window-is-unknown)); the error code itself is unchanged. A `send_handoff` also reports this when the target window's status has slipped back to `unknown` between an approval decision and delivery. Does not apply to `close_window`. `send_chat` / `broadcast_chat` never return this: a post to an `unknown` window waits in its inbox (`"queued"` in [read_chat](#read_chat)'s `deliveries`), since a chat post may still reach other targets
- **approval_denied**: An approver decided against a queued `interrupt_window` / `close_window` / `send_message` / `send_handoff` / `send_chat` / `broadcast_chat` request
- **approval_timeout**: A queued request was still undecided when the approval deadline elapsed (`[limits].approval_timeout_secs`, default 60s; see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)). A request to open a window (`open_window`, a `send_handoff` from a `profile`, `open_task_window`) whose requester's CLI dropped the call at the end of its turn (see the detection `[requests]` table) is withdrawn and answered this way too: no window was opened
- **interrupt_unsupported**: `interrupt_window` from an AI was refused because the window's profile gives an AI no way to interrupt its CLI (Codex CLI); nothing was written. The user's own interrupt is never refused
- **requester_interrupted**: The request's own window was interrupted by an AI while the request waited for an approver, so it was withdrawn and nothing was done
- **approval_not_pending**: An `approval_decision` named a `request_id` that this connection is not (or is no longer) waiting to decide -- already settled, withdrawn, or never issued to this connection (see [Approval Flow](#approval-flow))
- **no_composer**: Nothing was sent, because the profile declares no way to hand its CLI a request at launch (no `launch_prompt` in its `[delivery]` table; see [Launch profiles](../profiles/README.md#delivery)). A `send_handoff` addressed to a `profile`, and an `open_task_window` for a task that carries an instruction, return it before anything starts; the code keeps its original name. `send_message` never returns it: a window that takes no text this way answers `undelivered_no_delivery` (see [send_message](#send_message))
- **rate_limited**: A `status_event` was discarded because the daemon-wide rate limit was exceeded (the connection stays open; retry after a short delay)
- **auth_failed**: A `status_event`'s `token` field was missing, malformed, or did not match the target window's token (see [StatusEvent Authentication](#statusevent-authentication)); also returned for an `identify` call whose token was malformed or did not match the target window's own token -- a credential mismatch of this kind counts toward a per-connection limit of 3 before the connection is closed, while a malformed token or an unknown `window_id` does not (see [Requester Identification](#requester-identification)); also returned for a `register_approver` whose peer is not running the executable named by `--approver-exe`, or when the daemon was started without `--approver-exe` at all -- this refusal counts toward no limit and leaves the connection open (see [Approver Registration](#approver-registration))
- **invalid_state**: A connection tried to claim an origin that conflicts with one it already established -- a second `identify` for a different window, or `register_approver` on a connection that already identified as a window (see [Requester Identification](#requester-identification)); also returned by `open_task_window` when the task it names already has a bound window that is still alive (see [open_task_window](#open_task_window))
- **plan_not_found**: `update_plan` / `get_plan` / `delete_plan` / `open_task_window` was called by a window outside the plan entirely -- neither its owner nor bound to any of its tasks -- or named a `plan_id` that does not exist at all; both cases answer identically, so a caller outside a plan cannot tell it apart from one that was never created
- **not_plan_owner**: `update_plan` / `delete_plan` / `open_task_window` was called by a window that is not the plan's owner, or by the owner window while it is no longer alive (closed, or its child process has exited)
- **plan_not_approved**: `open_task_window` was called by the plan's living owner while the plan's review is still pending; no window is opened. Once an approver approves the plan, a one-line notice starting with `[yagura plan]` is handed to the owner's CLI and the call succeeds (see [Confirming a plan](plans.md#confirming-a-plan))
- **profile_disabled**: `open_window` (with `profile`), `send_handoff` (with `profile`) and `open_task_window` refuse this way when the launch profile they would open a window from has been switched off in the user's settings (see [Disabling a profile](../profiles/README.md#disabling-a-profile)). This is not an argument error, so repeating the same request reworded will not help
- **profile_not_found**: `open_window` (with `profile`), `send_handoff` (with `profile`) and `open_task_window` refuse this way when the launch profile they would open a window from is not one [list_profiles](#list_profiles) reports -- no such file, an identifier of the wrong shape, a file that cannot be read as a profile, or a profile carrying no argv for the daemon's operating system. Nothing is started. Call `list_profiles` for the identifiers this host can launch
- **ticket_not_found**: `get_ticket` / `update_ticket` / `add_ticket_result`, or a `create_plan` with `ticket`, named a ticket number the calling window's workspace does not hold -- one that does not exist, or one of another workspace; both cases answer identically. A `create_plan` refused this way creates no plan
- **tickets_not_configured**: `create_ticket` / `update_ticket` was refused because the user has no `tickets.toml` defining the board's states and fields. Reading still works: `list_tickets` returns the summaries with `config_error` in place of `config`
- **tickets_config_invalid**: `create_ticket` / `update_ticket` was refused because the user's `tickets.toml` could not be read as valid settings; the `message` names why, without quoting the file. Reading still works, as for `tickets_not_configured`
- **internal**: A contract violation between the daemon and `yagura-mcp` (an undecodable or unexpected response), or any other failure that is not the caller's fault

## Delivery Guarantees

`yagura-mcp` sends every tool call as a single request over an IPC connection to the daemon and delivers it **at most once**: if the connection needs to be re-established (for example, after a transport-level failure), a request is only ever retried when the failure happened *before* that request's bytes had finished being sent. Once a request has been fully sent, any later failure (including one while waiting for the reply) is surfaced as an error to the caller and the connection is discarded — the request itself is never resent. This matters most for `interrupt_window`, `close_window`, `send_message` and `send_handoff`: a caller that sees an error for one of these calls can be sure the operation did not silently run twice, but should treat "did it happen at all" as unknown and re-check the window's state (e.g. with `get_window_status`) before retrying. For `send_handoff`, an error on this call also means any window it opened from a `profile` for this attempt has already been cleaned up by the daemon (see [send_handoff](#send_handoff)) -- there is nothing left to close by hand. A call the MCP client cancels (`notifications/cancelled`) is answered with an error at once, and its connection is dropped rather than reused; the daemon reads that as the requester leaving, so a gated call still pending approval is withdrawn. An envelope a `send_message` or `send_handoff` already put in an inbox stays there: look for its `id` in [`get_window_status`](#get_window_status)'s `queued_ids`, and take it back with [`withdraw_message`](#withdraw_message). As with any other error, re-check the window before sending the same text again, and say in the text that it is a resend: `yagura-mcp` tells you when the daemon restarted under a call, and the earlier one may already have reached the receiver.

## Approval Flow

`open_window`, `close_window`, `interrupt_window`, `send_message`, `send_handoff`, `send_chat` and `broadcast_chat` are gated. Every connection to the daemon has an *origin*: `External` until it proves otherwise, `Window(id)` once it successfully identifies as a window, or `Approver` once it registers as one -- see [Requester Identification](#requester-identification) and [Approver Registration](#approver-registration). `yagura-mcp` identifies itself automatically, so a caller going through this MCP server is `Window(id)` whenever it runs inside a yagura window, and stays `External` otherwise; the GUI's own control connection registers as an approver.

The gate's decision, in order, for `close_window` / `interrupt_window` / `send_message` / `send_handoff` (chat's own decision is a variant of the same shape, described after this list):

1. The target window's status: `interrupt_window`, `send_message` and `send_handoff` (when addressed to an existing `window_id`) are refused outright as `status_unknown` when the target's status is `unknown`, regardless of origin or the playbook. For `send_handoff` this check runs again, against the window's then-current status, once an approval decision comes back -- see [send_handoff](#send_handoff). `close_window` is exempt.
2. The target window must have a way to receive the text: a `send_message`, and a `send_handoff` addressed to an existing `window_id`, are answered `undelivered_no_delivery` when the window was not opened from a profile that declares how its CLI is handed text (see [How the daemon delivers text](#how-the-daemon-delivers-text)), regardless of origin or the playbook; nothing is sent and nobody is asked. An AI's `interrupt_window` is refused as `interrupt_unsupported` when the window's profile gives an AI no means to interrupt its CLI (see [interrupt_window](#interrupt_window)). `close_window` is exempt.
3. The requester is the target window itself -> allowed immediately, no approval needed (see [Requester Identification](#requester-identification) for who this covers). Since `send_handoff` addressed to a `profile` always opens a brand-new window, the requester can never be that window, so this step never applies to that form.
4. The requester is a registered approver -> allowed immediately.
5. The active playbook's `[approval].auto_approve` lists the tool -> allowed immediately.
6. Otherwise the request is queued for a human decision -- unless no approver is currently connected, or the configured playbook failed to load, in which case it is refused outright as `approval_required` instead of being queued (see [Troubleshooting `approval_required`](#troubleshooting-approval_required)). For a `send_handoff` addressed to a `profile`, the decision is made once, up front, before anything is started, and covers the start and the envelope together (see [send_handoff](#send_handoff)).

`open_window` has no target window either -- the window does not exist yet -- so it runs steps 4 to 6 only: a registered approver (the GUI) is never asked to approve opening a window, and every other origin, `External` included, is allowed when `[approval].auto_approve` lists `open_window` (the shipped default does) and is otherwise queued for a human decision. The one gate covers every way an agent can start a window: `open_window` itself (both the `command` and the `profile` form), a `send_handoff` addressed to a `profile`, and `open_task_window`. For a `send_handoff` addressed to a `profile`, one decision covers the start and the envelope, made before the process exists: it passes on its own only when the policy lets both `open_window` and `send_handoff` through, and is otherwise asked once. For `open_task_window` the plan's own checks (the owner's window, an approved plan) come first and are unchanged. The playbook cannot make an agent's `command` safe: a window runs whatever it is given, outside the agent's own CLI permission prompts, which is why removing `open_window` from `auto_approve` makes every start wait for you.

`send_chat` and `broadcast_chat` have no single target window to run steps 1, 2 and 3 against -- they post to a room rather than hand text to one window, so the delivery check of step 2 is not their business either -- so they follow a variant: every origin runs steps 4 and 5 like any other tool, an unidentified (`External`) connection included, so a chat post skips the dialog only while the tool is listed in `[approval].auto_approve` -- which the shipped default does for `send_chat` and does not for `broadcast_chat`.

A queued request is sent to every registered approver as one entry of `ApprovalRequested`, carrying the tool, the requester (a window's name, or "external"), the target window's name and status **as observed at submit time** (omitted for `open_window`, a `send_handoff` addressed to a `profile`, `send_chat` and `broadcast_chat`, which have none), and -- for `open_window`, `send_message`, `send_handoff`, `send_chat` and `broadcast_chat` -- the full text body (the handoff envelope, for `send_handoff`; the chat envelope, for the two chat tools; for `open_window`, lines naming what would start: `command: <argv>` or `profile: <profile id>`, then `cwd: <directory>` and, for a window a handoff or a plan task opens, `for: handoff` or `for: plan task "<task id>"`; control characters in any of them are shown escaped; for a `send_handoff` addressed to a `profile`, the same lines followed by the envelope). An `open_window` whose description would exceed the approval text limit is refused as `invalid_argument` rather than shown with its tail hidden. An approver settles it with a `request_id` and `approved: true`/`false`; every other approver is then told the dialog is no longer actionable. The caller's own reply is one of:

- **Approved**: the daemon re-checks the status gate against the window's *current* status -- not the one recorded when the request was submitted -- immediately before running the operation. A window that went `unknown` while the request sat waiting is refused as `status_unknown` even though it was approved: an approval decided against a stale status is not a licence to fire blind at whatever the window is doing now. `send_chat` / `broadcast_chat` have no single target to re-check this way -- an approved chat post is recorded and delivery begins for every target: the post waits in each window's inbox, and one whose window has ended meanwhile is recorded in [read_chat](#read_chat)'s `deliveries` as `"undelivered_unknown"` rather than failing the call.
- **Denied**: `approval_denied`.
- **The deadline elapses first** (`[limits].approval_timeout_secs`, default 60s -- see below): `approval_timeout`.
- **The requester's own connection disconnects** while the request is still pending: it is withdrawn and nobody is left to receive a reply. A request to open a window is also withdrawn when its requester's CLI drops the call at the end of a turn; that call is answered `approval_timeout` on its still-open connection.
- **An AI interrupts the requester's own window** while the request is still pending: it is withdrawn and answered `requester_interrupted`, and nothing is done (see [interrupt_window](#interrupt_window)).

Reconnecting as an approver (the GUI does this on every fresh control connection, including after a reconnect) returns every currently outstanding request in its response's `pending` field, ordered oldest first (submission order), so a GUI that reconnects mid-dialog gets its queue restored rather than starting empty. There is a cap on how many requests may be pending at once, daemon-wide and per caller (one MCP session, or one direct IPC connection); past it, submitting a new one is refused as `resource_exhausted` rather than displacing an older request. This is separate from `send_handoff`'s own two-stage limit on handoffs that have not been answered yet (see [send_handoff](#send_handoff)): a `send_handoff` still waiting on an approval decision counts toward both its own limit and this pending-approval cap at the same time.

**Concurrency from an MCP client's point of view**: `yagura-mcp` carries each tool call over a daemon connection of its own, at most four at once (see [`wait_for_status`](#wait_for_status)'s blocking semantics), so a gated call that is pending -- or a `send_message` / `send_handoff` waiting on a wait its receiver left registered -- does not hold up the same MCP client's other tool calls unless four calls are already outstanding; a call beyond that waits for one of them to end. The daemon counts the limits on pending approvals and outstanding handoffs per caller, across all of one MCP session's calls; the limit on pending waits (`wait_for_status`, `wait_for_report`) applies to each of those connections separately.

**GUI presentation**: the approval dialog cannot be dismissed -- Escape and clicking outside it do nothing, only Approve or Deny close it, and the request stays pending regardless. If another dialog (add window, confirm, rename, restore) is already open, the approval dialog waits behind it and opens once that one closes, rather than stacking. A request that expires or is withdrawn without ever having been shown (queued behind another dialog, for instance) is instead surfaced afterward as a dismissible banner summarizing how many requests expired unseen.

**What an approval request does not carry**: the launch attributes of a `send_handoff` addressed to a `profile` -- its `model` and `effort` -- are not part of what an approver is shown, and are not part of the envelope either. They decide how the new window's CLI is started, not what is being asked of whoever ends up reading it; the tool, the requester, the target window and the full text body remain what the decision is made on.

## Playbooks: Approval and Limits

`open_window`, `close_window`, `interrupt_window`, `send_message`, `send_handoff`, `send_chat` and `broadcast_chat` are the operations the approval gate covers. The daemon's default is **fail-closed: with no playbook resolved, none of the seven is ever auto-approved**, regardless of the target window's state -- though, as described in [Approval Flow](#approval-flow), a request from the target window itself or from a registered approver bypasses the playbook entirely and is never affected by this section.

A playbook is a TOML file that relaxes what still reaches a human. The daemon resolves the playbook path in two stages, in order:

1. `--playbook <path>` (command-line flag)
2. the `YAGURA_PLAYBOOK` environment variable

There is no executable-relative search: the daemon never infers trust from the shape of the filesystem (an install directory vs. a build directory, a co-located `Cargo.toml`, and so on). The caller is expected to name the file explicitly.

**The GUI seeds its playbook, then passes it explicitly.** At every start the GUI writes yagura's default playbook -- the one the repository ships at its root, which auto-approves only `open_window` and `send_chat` -- into the configuration root (Windows: `%LOCALAPPDATA%\io.github.satoh-y-0323.yagura\playbook.toml`; macOS: `~/Library/Application Support/yagura/playbook.toml`; Linux: `$XDG_CONFIG_HOME/yagura/playbook.toml`, falling back to `~/.config/yagura/playbook.toml`), **but only when no file exists there yet**. An existing `playbook.toml` is never read, rewritten, or overwritten by the GUI, so your edits survive restarts, upgrades, and reinstalls. The GUI then spawns the daemon with `--playbook <config-root>/playbook.toml` when that file exists; when it does not -- the configuration root could not be resolved, or the write failed -- it starts the daemon with no `--playbook` flag at all and the fail-closed default above applies. No manual copying is needed for the normal, GUI-launched case.

Two consequences of that shipped default are worth stating explicitly:

- **`auto_approve` still does not distinguish callers, only tools.** Listing a tool there means *any* non-self, non-approver requester -- a script driving another MCP client just as much as anyone else -- skips the approval dialog for that tool. The shipped default is `["send_chat", "open_window"]`, so out of the box every `close_window` / `interrupt_window` / `send_message` / `send_handoff` / `broadcast_chat` request from another window or an external client reaches a human through the GUI's approval dialog, while `send_chat` does not (its reach is already bounded to chat participants, see [send_chat](#send_chat)) and neither does `open_window` (agents start windows as they always have; remove it to be asked each time); add entries to `[approval].auto_approve` yourself if you want a specific tool to bypass the dialog for every non-self, non-approver caller.
- **A daemon started outside the GUI is not covered.** The seeding happens in the GUI process, so a `yagura-daemon` you start yourself -- from a shell, a service, a test harness -- receives no playbook at all and denies every non-self, non-approver destructive call, even when the configuration root already holds one.

Running the daemon directly from the command line is otherwise unaffected by this fixed path -- pass `--playbook <path>` or set `YAGURA_PLAYBOOK` yourself, pointing at whichever file you want, including the seeded `<config-root>/playbook.toml` or the repository's own `playbook.toml`.

```toml
[approval]
auto_approve = ["send_chat", "open_window"]          # these two by default; list tool names to skip the dialog for them

[plans]                                              # optional; a missing table or key keeps the built-in default (true)
require_approval = true                              # whether a new plan waits for the operator's approve_plan before its tasks can open

[limits]                                             # optional; omitted fields (or the whole table) keep the built-in default
approval_timeout_secs = 60                           # deadline for a pending approval, in seconds (1..=600)
interrupt_bytes = [3]                                # byte sequence interrupt_window writes to the PTY of a
                                                      # window whose profile declares no [interrupt]
wait_timeout_max_secs = 600                          # upper bound accepted for wait_for_status's timeout_sec
max_queued_per_sender = 8                            # envelopes one sender may leave waiting in one window
                                                      # (1..=64)

[chat]                                                # optional; omitted fields (or the whole table) keep the built-in default
repetition_count = 4                                  # occurrences of the same room state that halt the room as a loop (1 or more)
repetition_window = 12                                # history window compared for repetition (1 or more)
repetition_distance = 6                               # maximum simhash Hamming distance still counted as the same state (0..=64)
monologue_max = 3                                     # consecutive posts from the same window before the room halts (1 or more)
turn_message_max = 30                                 # AI posts allowed since the last user post before the room halts (1 or more)
silence_timeout_secs = 300                            # seconds of no reply before the room needs the user (1..=86400)
history_max = 5000                                    # messages retained per room (1 or more)
intro = "..."                                         # announcement a participant window's CLI reads when it starts; empty disables it
exclude_detections = ["generic-shell"]                # detection definition names excluded from chat participation;
                                                       # a window not opened from a profile that declares [delivery] never participates either way
```

- `[approval].auto_approve` lists MCP tool names (the external, stable tool names -- `open_window`, `close_window`, `interrupt_window`, `send_message`, `send_handoff`, `send_chat` and `broadcast_chat`) that may run without a human decision. There is no wildcard; each tool must be listed explicitly. An unrecognized tool name in the list is not an error -- it is logged as a warning and ignored, so a playbook stays forward-compatible with future tool additions. `send_keys`, a tool earlier releases had, is read with a warning that it is gone and approves nothing.
- `[plans]` is optional. `require_approval` defaults to `true` -- a missing `[plans]` table, or the table present with the key omitted, still means every new plan waits for the operator's `approve_plan`. Switching it from the GUI's Settings > Approvals tab (see [Changing approvals from the GUI](#changing-approvals-from-the-gui) below) takes effect at once, but only for a plan created after the switch -- a plan already in the ledger keeps the review state it started with. Editing the key directly in the file instead still needs a daemon restart to take effect. See [Confirming a plan](plans.md#confirming-a-plan) for what the gate refuses and how it is settled.
- `[limits]` is optional. Only the fields actually present override the daemon's built-in defaults; anything omitted (including the whole `[limits]` table) keeps the default.
- `[limits].approval_timeout_secs`, when present, must be within `1..=600`; a value outside that range fails the **whole playbook** to load, not just this key -- the same as any other schema violation. The daemon's own built-in default is 60s, so omitting the key has the same effect as writing `60`.
- `[limits].max_queued_per_sender`, when present, is how many envelopes one sender -- a window, or every caller that is no window, together -- may leave waiting in one window's inbox (see [How the daemon delivers text](#how-the-daemon-delivers-text)). A value outside `1..=64` (64 is the inbox's own capacity) does not fail the playbook: the daemon logs a warning naming the key and keeps the built-in default, 8.
- `[limits].interrupt_bytes` is what [`interrupt_window`](#interrupt_window) writes to a window whose profile declares no `[interrupt]` table, or that was opened without a profile; a profile's `[interrupt]` names its own keys (see [Interrupt](../profiles/README.md#interrupt)).
- `[limits].handoff_ready_timeout_secs`, `handoff_delivery_timeout_secs` and `chat_delivery_max` are deprecated: nothing is typed into a window any more, and a handoff does not wait for its envelope to be taken. Each still loads, whatever its value, with a warning naming it, and has no effect.
- `[chat]` relaxes the chat rooms' own limits and the intro text; every field is optional and defaults to the built-in value shown in the example above (`repetition_count` 4, `repetition_window` 12, `repetition_distance` 6, `monologue_max` 3, `turn_message_max` 30, `silence_timeout_secs` 300, `history_max` 5000, a non-empty `intro`, and `exclude_detections` of `["generic-shell"]`). Every numeric field is range-checked the same way the `[limits]` deadlines are -- a value outside the built-in range for any single field fails the **whole playbook** to load. **When the playbook fails to load for any reason (an out-of-range `[chat]` or `[limits]` value, an unknown key, or any other schema violation), chat itself is disabled for the whole daemon**: `send_chat`, `broadcast_chat` and `read_chat` all return `unsupported` until the daemon is restarted with a playbook that loads -- this is stricter than the other five gated tools, which merely deny individual requests when the playbook is broken.
- **Unknown keys anywhere in the file (an unrecognized section or field, including typos) make the whole file fail to load.** This is deliberate: a mistyped setting must be a loud failure, never a silently-ignored one.
- An empty file, or a file with `[approval]` omitted entirely, is valid TOML and loads successfully -- approval then auto-approves nothing, the same as no playbook at all.
- **The daemon does not exit when a playbook fails to load or the configured path does not exist.** It logs a warning and continues running with every non-self, non-approver destructive operation denied (fail-closed degrades gracefully; a broken playbook must never take down the windows it would have governed).
- `[approval].auto_approve` and `[plans].require_approval` take effect the moment they are switched from the GUI's Settings > Approvals tab (see [Changing approvals from the GUI](#changing-approvals-from-the-gui) below). Every other setting in this file -- and a change to either of those two made by editing the file directly, rather than through that tab -- is still read exactly once, at daemon startup: there is no hot reload for them, and changing them requires restarting the daemon.
- Operational parameters other than `approval_timeout_secs`, `interrupt_bytes`, `wait_timeout_max_secs`, `max_queued_per_sender` and `[chat]` (e.g. the close grace period, rate-limit burst/refill, `read_output` bounds) are built-in defaults and cannot be overridden by a playbook.

### Changing approvals from the GUI

The GUI's Settings panel (the gear button in the navigation rail) has an **Approvals** tab: a switch for each of the seven gated tools' `auto_approve` entry, and one more for `[plans].require_approval`. These two settings are the only ones this tab -- or anything else -- can change while the daemon keeps running; every other playbook setting is still read only at startup, as above. There is no MCP tool for this: only a registered approver, from the GUI, can switch either setting.

- **It takes effect immediately, for what happens next.** A tool switched to auto-approve stops raising the approval dialog for the very next request that needs it; switching `require_approval` off means the very next `create_plan` is not gated. Neither is retroactive: a request already waiting for a human decision when a tool is switched on stays waiting, and a plan already in the ledger keeps the review state it had when it was created (see [Confirming a plan](plans.md#confirming-a-plan)).
- **The file and the daemon's running decision are judged separately.** Pressing a switch rewrites the file only when the file's own current value disagrees with it, and changes (and logs) the daemon's running decision only when *that* disagrees with it -- the two checks do not depend on each other. A hand edit of the file is never picked up by a switch itself, and is only read the next time the daemon starts, so the file and the daemon's decision can drift apart in the meantime (and the tab can disagree with the file on disk) until either a switch or a restart brings them back together. Because the two checks are independent, a switch that only needs to catch the daemon's decision up to a file already hand-edited to match it changes the daemon's decision (and logs it) without writing the file again; one that only needs to catch the file up to a daemon decision a hand edit had not touched rewrites the file without the daemon's decision changing (and logs nothing, since nothing about the running decision changed).
- **The rewrite touches only the one thing requested.** The rest of the file -- comments, every other table, and its line endings -- is left exactly as it was. On Unix, writing the file also narrows the directory it is placed in, and the file itself, to owner-only access, the same way the GUI's initial seeding of the file does (see [Checking the configuration root](#checking-the-configuration-root)); on Windows this is a no-op, since NTFS inheritance from `%LOCALAPPDATA%` already scopes the directory to its owner.
- **Deleting the file** does not change the running daemon's own decision. It only means the next time the GUI starts, it seeds a fresh copy of the shipped default there again (see above); the running daemon still needs a restart to pick that copy up.
- **Every successful switch is logged as one line** naming the tool (or `plans`) and the new value it was set to. When the playbook was never configured, or it failed to load at daemon startup, the whole tab is read-only with a one-line notice instead of a switch to press. When the playbook loaded at startup but a switch cannot be carried out anyway -- the file has since become unreadable, or writing it back failed -- that one switch reverts and a one-line failure message appears at the top of the tab (the rest of the tab stays usable); the daemon logs a warning, and both the file and its own decision are left untouched.

### Troubleshooting `approval_required`

`approval_required` means the request was refused **before it could even reach a human**. The `message` field tells you which of two states produced it:

1. **`"no approver is connected, so <tool> is denied"`**: nobody is currently registered as an approver to decide the request -- usually because the GUI is not running, or its control connection has not finished registering (see [Approver Registration](#approver-registration)). This is the ordinary case, and it is not specific to a broken or missing playbook: as long as an approver *is* connected, a missing playbook, an empty one, or one whose `auto_approve` does not list this tool all land in the approval queue instead of being denied outright -- this message means literally nobody was there to decide it.
2. **`"the configured playbook failed to load, so <tool> is denied"`**: `--playbook` or `YAGURA_PLAYBOOK` resolved a path, but reading or parsing it failed. Check the daemon log (see [Debugging Connection Issues](#debugging-connection-issues)) for a warning line -- grep for `failed to load the playbook`, which names the exact error (a missing file reads as an I/O error, an unknown key or wrong-typed value as a TOML parse error).

For a `send_handoff` addressed to a `profile`, both messages can also appear for this same reason **before the window is ever opened** -- this pre-launch check (see [send_handoff](#send_handoff)) asks the same question the gate would ask after opening the window, just earlier, so it never spawns a process it already knows it cannot deliver into. This is separate from a `profile` that [list_profiles](#list_profiles) does not report, which is always `profile_not_found` regardless of whether an approver is connected or the playbook loaded.

### Troubleshooting `status_unknown` and `child_exited`

Both are reported the same way to every caller, regardless of origin or the playbook: there is no partial disclosure that hides a window's state from an external caller, since `get_window_status` already lets anyone read it.

- **`status_unknown`** applies to `interrupt_window`, `send_message` and `send_handoff` (not `close_window`, which stays closable even when undetectable). For `send_handoff` this covers an existing `window_id` target directly, as well as the re-check made once an approval decision comes back for either a `window_id` or a freshly opened `profile` target -- see [send_handoff](#send_handoff). A window whose detection definition never matches its own output stays `unknown` forever, so `send_message` can never reach it -- not even a call from inside that same window. The way out is to assign a detection definition that actually matches the window's output (`open_window`'s `detection` argument; see [Detection definitions](../crates/yagura-daemon/detection/README.md)). A status reported with `yagura-daemon --emit-status` does not make such a window accept input: a report only marks a turn as open or closed, and a closed turn leaves the status to the screen (see [Structured sources](../crates/yagura-daemon/detection/README.md#structured-sources---emit-status)).
- **`child_exited`** is checked before the approval gate for `interrupt_window` and for a `send_handoff` addressed to an existing `window_id`, so a target that has already exited never generates an approval request at all.

#### Why a window is `unknown`

Every `unknown` the daemon reports carries a reason. It reaches you as `unknown_reason` in [list_windows](#list_windows) and [get_window_status](#get_window_status), as the phrase in a `status_unknown` refusal's `message`, and beside the status in the GUI. The six reasons and what each asks of you:

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

- **`identify { window_id, token }`** claims the connection as that window, proven with the window's own per-boot status token (see [StatusEvent Authentication](#statusevent-authentication) for how the token reaches a process). `yagura-mcp` sends this automatically, once, on every fresh connection it opens, using the `YAGURA_WINDOW_ID` / `YAGURA_STATUS_TOKEN` environment variables it inherits from its own process's environment (both non-empty), or else the credentials file named by its `--credentials <path>` argument, which the bundled profiles in `launch` mode put in the server definition. Either is populated only when `yagura-mcp` is itself running inside (or spawned from within) a window the daemon opened. A bridge started outside any such window -- a person's own MCP client, not launched by an agent CLI running in a yagura window -- never sees these variables and stays `External` for its whole lifetime. The same happens inside a window when the MCP client does not pass its own environment on to the server it spawns: Codex CLI assembles an stdio server's environment from an allow-list, so both variables have to be forwarded explicitly -- by the `env_vars` override a bundled profile's `[mcp]` table injects (see [Windows yagura Opens](#windows-yagura-opens)), or by its own configuration (see [Configuring Codex CLI](#configuring-codex-cli)). Claude Code passes its environment through unchanged, but the bundled `claude-code` profile does not rely on that: it names the credentials file in the server's `--credentials` argument. A WSL window's variables cross into the distribution only because the daemon names them in `WSLENV` (see [docs/wsl.md](wsl.md#environment-variables)), and back out to the Windows bridge a WSL Codex CLI starts only because `wsl-codex-cli` names them in that server's own `WSLENV`.
- **`register_approver`** claims the connection as an approver (see [Approver Registration](#approver-registration)).

**Who this identifies**: the token reaches the window's hooks and bridge either through the environment of the window's *whole process tree* (`env` mode) or through a credentials file whose path only their command lines carry (`launch` mode). In `env` mode the self-window exemption (see [Approval Flow](#approval-flow)) is granted to any process running inside that tree that can read its own environment and open a connection to the daemon; in `launch` mode only a process that is handed the file's path, or that can read the file as the same OS user, can use it. This does not exceed what such a process could already do directly -- write to its own PTY, or terminate anything sharing its Job Object / process group -- so it is accepted as within the daemon's existing trust boundary.

**Idempotence and conflicts**: re-asserting the same claim (the same `window_id`, or registering as approver again) succeeds and leaves the origin unchanged. Claiming a *different* origin than the one already established -- a second `identify` for another window, or `register_approver` on a connection that already identified as a window -- is refused as `invalid_state`, and the origin is left untouched; `External` is the only origin from which either claim may still be attempted. `identify` failures that are credential mismatches (a well-formed token that does not match the target window's) count toward a per-connection limit of 3 before the connection is closed; a malformed token and an unknown `window_id` do not count.

**A bridge names the profile it speaks for.** The bundled profiles' bridges pass `--profile <id>`, and `identify` carries it as a declaration. A window opened from a launch profile that gives its CLI hooks (a `[hooks]` table, or hook entries its registration writes) accepts a bridge as itself only when the bridge declares that window's own profile; a bridge that declares another, or none -- a CLI nested in the window, or a registration written before this declaration existed -- is refused (`profile_mismatch`) and stays `External`. A plain shell window, and one opened from a raw command, accept a bridge as before. The declaration is the bridge's own word, a mark that keeps yagura's own parts from claiming the wrong window, not a check on a process that holds the credentials (see [Credentials](../profiles/README.md#credentials)).

**A claim can be refused although its credentials hold.** A window whose launch overlay has not been made yet (a profile with `[launch_overlay]`, such as the WSL Grok Build profile) refuses every claim as `launch_overlay_pending`. Each refusal leaves the bridge `External`, with an `identity_warning` naming why. A claim that is accepted speaks for the window whatever process made it: see [Credentials](../profiles/README.md#credentials) for which processes the window's credentials reach.

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

An interrupt by keys -- the user's own, an AI's under a profile whose `[interrupt]` declares `ai = "keys"`, and either one in a window with no profile or no `[interrupt]` table -- writes key presses to the window's PTY input. A profile's `[interrupt]` names its keys by word (`escape`, `ctrl_c`), and each is written the way the same key pressed in the window would be, in the window's current input mode (see [How Keys Reach the Program](keymap.md#how-keys-reach-the-program)); `[limits].interrupt_bytes` is written as raw bytes (`0x03`, Ctrl-C, by default). Whether a key actually interrupts the target CLI depends on that CLI's own input handling -- under ConPTY on Windows, Ctrl-C is turned into the console interrupt event before the child ever sees it, and on macOS and Linux the pseudo-terminal's line discipline turns `0x03` into `SIGINT` for the foreground process group unless the program has switched that off (as a program reading raw input does). A CLI that does not react to the key will not be interrupted; a profile's `[interrupt]` is where to name the key it does react to.

An AI's interrupt in the hook form (`ai = "hook"`) writes nothing: it marks the window's open turn, and the CLI's own hooks answer its next stop event with a stop (see [interrupt_window](#interrupt_window)). It therefore takes effect only once the CLI reaches such an event -- a tool call, for the bundled profiles -- and only in a window whose hooks reach the daemon; `stop_requested_unconfirmed` says the daemon has not seen them yet.

## StatusEvent Authentication

`status_event` (the message the daemon's `--emit-status` helper, and by extension a child CLI's own hooks, use to report status) requires a `token` field. The daemon rejects any event whose token is missing, does not match, or is the wrong length — all as `auth_failed`.

**Mechanism**: tokens are **per window**. When the daemon opens a window it generates a fresh 128-bit token (from the OS RNG) for that window alone, never logs it, and hands it to that window's CLI in one of two ways, chosen by the launch profile's `[credentials]` mode (see [Credentials](../profiles/README.md#credentials)). In the default `env` mode it injects the token into that child's environment as `YAGURA_STATUS_TOKEN`, together with that window's own id as `YAGURA_WINDOW_ID`, and does not persist it; a child CLI's hooks (or any code running inside that child's process tree) inherit both environment variables. In `launch` mode the child's environment holds neither: the daemon writes both to a file only the window's owner can read, removes it when the window's child exits, and the profile's hook commands and bridge definition carry its path as `--credentials <path>`. Either way, a hook invokes `yagura-daemon --emit-status`, which reads the pair (both environment variables when neither is empty, otherwise the `--credentials` file) and forwards it as the `status_event`'s `token` and `window_id`. `YAGURA_WINDOW_ID` is only a matching key that tells the daemon which window's token to compare against; knowing it grants no authentication on its own, since a process that only knows another window's id still cannot obtain that window's token. The daemon resolves the event's target window first and then compares the token against *that window's* token, so:

- A process with no route to such an environment variable or credentials file cannot authenticate at all. In `launch` mode that includes a CLI started by one of the window's tools, which inherits neither: its hooks and bridge are outside any window.
- A process that holds one window's token can only report status **for that window**. Reading the environment of window A does not let anything speak for window B — an event aimed at B with A's token is rejected as `auth_failed` and leaves B's status untouched, regardless of which `window_id` accompanies it.

**`--emit-status` window attribution**: the helper reads the window's id from `YAGURA_WINDOW_ID` (or from the `--credentials` file) and sends it as the event's `window_id`, so a hook running anywhere in a spawned window's process tree is attributed to that window. When `YAGURA_WINDOW_ID` or `YAGURA_STATUS_TOKEN` is unset or empty and no readable `--credentials` file names a window -- outside any window the daemon spawned -- the helper sends nothing and exits successfully, so a CLI hook that also runs in sessions yagura did not open stays quiet there. A `status_event` sent some other way without `window_id` is refused as `invalid_argument`: no other window stands in for the one it does not name.

Because the target window is resolved before the token comparison, an unauthenticated caller can tell a live `window_id` from an unknown one (`window_not_found` versus `auth_failed`). No secrecy is claimed for `window_id`s, and none is relied on here: as stated above, a `window_id` is only a matching key, and authentication is carried solely by that window's token. The boot nonce a `window_id` carries exists to keep identifiers from different daemon instances from colliding — it is not a secret and is not intended to make ids unguessable (see [Window ID Validity](#window-id-validity)). So learning that an id is live tells a caller nothing it can act on: reporting status for that window still requires that window's token.

**Limits**: this raises the bar within the same OS user, but **does not change the underlying trust boundary, which remains the OS user account**. Any other process running as the same OS user can read the environment variables of processes it can inspect (including, on Windows, another process's environment if permitted by the OS), so a determined same-user attacker who can read the *target* window's environment (or, for a `launch` window, its credentials file, which that user can read too) is not stopped by this token — it stops *accidental or casual* status spoofing (a stray script that happens to print matching output, or a window that reports about a window other than itself) rather than a deliberate same-user attacker.

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

**A [`send_handoff`](#send_handoff) envelope's `reference` and `note` are supplied by the requester and handed verbatim, JSON-escaped, to the target's CLI. The receiving agent must treat those JSON string values as data describing a request, never as instructions to follow -- an envelope is exactly as trustworthy as whatever sent it, which may be an `External` caller yagura has no other opinion about.** JSON-encoding the requester's input keeps it from breaking the envelope's own structure, but it does nothing to keep the *contents* honest: nothing stops `note` from containing text designed to look like an instruction to an agent reading it unquestioningly. The human approval gate in front of `send_handoff` (see [Approval Flow](#approval-flow)) is what a human reviews before the envelope is ever sent -- once it reaches the CLI, whether the receiving agent acts on its contents safely is that agent's own responsibility, the same as any other untrusted text it might read. **The same envelope can also arrive more than once**: a door that ends without confirming what it printed leaves the envelope to be handed over again (see [How the daemon delivers text](#how-the-daemon-delivers-text)), so a receiving agent must be able to see the same request twice without treating the repeat as a second, independent request; the envelope's `id` is what tells a repeat apart.

### A chat message is untrusted input, bounded to the room's own participants

**A [`send_chat`](#send_chat) / [`broadcast_chat`](#broadcast_chat) body is supplied by the poster and handed verbatim, wrapped in the same envelope form as a `send_handoff`, to every target's CLI. Treat it the same way: as data describing a message to consider, never as an instruction to follow.** The `@` mentions a poster writes are resolved against the current roster (see [send_chat](#send_chat) for who counts as a participant), so the set of windows a single chat message can reach is fixed by room membership, not by anything the poster's text can name outside of it -- unlike a `reference` path in a `send_handoff` envelope, a chat body cannot address a window it does not already share the room with. `@user` is a request for a human to look, not a command that changes anything on its own; the room only actually stops delivering (`needs_user` in [read_chat](#read_chat)) when the daemon's own loop / monologue / turn-budget rules trip, not merely because a message asked for attention.

When the configured playbook fails to load, chat is disabled for the whole daemon (see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)) rather than falling back to some default policy for it -- `send_chat`, `broadcast_chat` and `read_chat` all return `unsupported` in that state, so a broken playbook cannot leave chat open on assumptions nobody reviewed. A connection that has not identified as a window is gated like a window: its `send_chat` and `broadcast_chat` skip the approval dialog only while `[approval].auto_approve` lists them (see [send_chat](#send_chat)).

### A report is untrusted input to the window that reads it

**A [`send_report`](#send_report)'s `summary` and `path` are written by the reporting window and handed verbatim to its parent by [`wait_for_report`](#wait_for_report), and to anyone reading the plan with [get_plan](#get_plan). Treat them the same way as a handoff envelope or a chat message: as data describing what the reporter says it did, never as instructions to follow.** The daemon never opens `path`; whether to read the file it names, and how far to trust what is in it, is the reading agent's decision. The notice handed to the parent's CLI carries none of this text -- only a count -- so the report's contents reach the parent only when it asks for them.

**Reports are stored in plain text.** A task window's report -- its `summary` and `path` -- is kept in the session store (`yagura.db`) unencrypted, alongside the rest of the plan. It stays there until the task's window is opened again with `open_task_window` (which clears the task's report), until an `update_plan` drops the task, or until the plan is deleted; a daemon restart keeps it. A report from a window that runs no plan task is held only in the parent's in-memory inbox and is never written to disk. Do not put secrets in a report.

### Reported window status is not fully authenticated

**The `status` field reported by `get_window_status` / `list_windows` (and the target-reached signal from `wait_for_status`) is only as trustworthy as [StatusEvent Authentication](#statusevent-authentication)'s guarantee: an unauthenticated send is rejected by the window's own token, but a same-user process that can read *that window's* environment can recover its token and forge a status for it. Do not use it as the sole basis for autonomous approval or input-injection decisions.**

Treat the status as an advisory hint for a human operator, and require an independent check (or human confirmation) before any action that grants approval, injects input, or is otherwise destructive. The approval gate in front of `interrupt_window` / `close_window` / `send_message` / `send_handoff` / `send_chat` / `broadcast_chat` (see [Playbooks: Approval and Limits](#playbooks-approval-and-limits)) is the structural control for the destructive tools; the status field itself carries no such guarantee.

## When a Window's Holder Ends Unexpectedly

Each window's terminal is kept by a process of its own, the window's holder (`yagura-holder`). On macOS and Linux the holder starts the window's command through a small watcher process (`yagura-watcher`, installed beside it), one per window, which is the command's parent and ends the window's processes when the window closes. Windows has no watcher: the window's Job Object ends its processes.

If a window's holder ends unexpectedly (it crashes or is killed), its watcher reads the window's processes, then hangs up the window's terminal and ends them as [close_window](#close_window) would. If the watcher ends unexpectedly instead, the holder ends the window's processes (on macOS, only those it can still reach from the window's command while that command runs) and the window ends. While the holder runs, it resumes a watcher that something outside yagura stopped (for example with `SIGSTOP`).

If both end before the window's processes are ended, yagura ends only what it can still reach from the window's command while that command runs, as soon as both have ended (or the next time yagura starts, if yagura was not running then); what it has read stays in its sweep even if their parents end meanwhile. On Linux with a systemd user manager, systemd also ends the window's scope once the holder and the watcher have both ended; on macOS and on Linux without systemd, the rest may be left running. The macOS limit described under [close_window](#close_window) applies here too. A process the window's command handed to a service outside the window (for example a CLI's own background server) is not part of the window and is never ended by it.

If something outside yagura stops the watcher after the holder has ended, the window is reported `exited` meanwhile. Its processes, including the window's command, keep running until the watcher is resumed or ends. If the watcher itself is stuck inside the operating system while the holder runs, the window is not reported `exited` until the watcher returns. If the watcher cannot be started, or speaks another version than the holder, the window does not open and `open_window` fails as `spawn_failed`.

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

8. A WSL profile starts the Windows `yagura-mcp.exe` through WSL interop. If Codex CLI's `/mcp`
   shows yagura as failed in a WSL window, run `/mnt/c/Windows/System32/cmd.exe /c ver` inside the
   distribution: `Exec format error` means interop is off, and no window's yagura MCP server can
   start until it is back. `wsl --shutdown` from Windows restores it only until WSL next restarts,
   so expect to run it again. This has been seen on Ubuntu 24.04 with systemd enabled in WSL, and
   may have other causes.
