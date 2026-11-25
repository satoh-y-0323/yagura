# WSL Guide

This guide explains how to run a command inside a WSL distribution from yagura, and what the
verification of that path actually observed.

> **There is no WSL mode.** `wsl.exe` is an ordinary Windows program, so a "WSL window" is just a
> window whose command happens to be `wsl`. yagura has no code that recognizes `wsl`, rewrites its
> arguments, or translates paths for it. What yagura contributes to this page ends there: the
> bundled WSL profiles exec the CLI directly through a fixed wrapper (`bash -lic 'exec "$0" "$@"'`)
> without going through a shell, so a launch option's value and the daemon's MCP injection reach the
> CLI's own argv unchanged -- see [profiles/README.md](../profiles/README.md#launch-options) and
> [mcp-setup.md](mcp-setup.md) for what each carries. The observations below still come from
> `wsl.exe` and the distribution's own shell, not from yagura. The behavior below was observed on
> Windows 11 with WSL2 (Ubuntu) using `wsl -- bash`.

## Opening a WSL Window

The command field is part of the form only while `Launch profile` is set to `Custom…`, the last
entry in that select; picking a profile replaces the field with a read-only summary of what that
profile launches. The dialog opens on the profile launched last, or on the first profile offered,
so `Custom…` has to be selected first unless no profile is offered at all.

The command field of the "add window" form (`Command (one token per line)`) takes **one argv token
per line**. It is not a shell command line: yagura never splits on spaces and never interprets
quotes, so `wsl -- bash` is three lines:

```
wsl
--
bash
```

Blank lines are dropped and each remaining line is trimmed, so the three lines above become the
argument list `["wsl", "--", "bash"]` and nothing else. The same text typed on one line is one
token — a program name containing spaces — not three arguments.

With those three lines the window opens and the distribution's shell prompt appears in the pane;
input typed into the pane (for example `pwd`) is answered by that shell.

A command typed this way is a raw argv: it gets no MCP injection (see
[MCP from a WSL Window](#mcp-from-a-wsl-window)). To run a CLI with yagura's MCP tools available,
open it from the `WSL: Claude Code`, `WSL: Codex CLI`, `WSL: GitHub Copilot CLI` or
`WSL: Muse Code` profile instead (`WSL: Grok Build` opens a window the same way
but needs its own MCP registration -- see
[MCP from a WSL Window](#mcp-from-a-wsl-window)).

## The Launch Form of the Bundled WSL Profiles

The WSL profiles start their CLI with

```
wsl -e bash -lic 'exec "$0" "$@"' <cli> <appended arguments...>
```

given to `wsl.exe` as separate argv elements. `wsl -e` runs `bash` directly, not through the
distribution's default shell; `-li` makes it both a login and an interactive shell, so both
`~/.profile` and `~/.bashrc` run, and the `PATH` they set together -- including an addition a tool
such as nvm makes only in `~/.bashrc`, which a plain login shell does not read -- is what finds the
CLI; and the fixed script `exec "$0" "$@"` replaces bash with the CLI, handing it
every later element -- launch options and the MCP injection -- as one argument each, unsplit and
unexpanded. This needs `bash` in the distribution, and the **Linux** build of the CLI installed
inside it: the Windows build on the host is not what runs. See
[profiles/README.md](../profiles/README.md#wsl-profiles).

One of them adds a step before that form:

- `wsl-grok-build` runs, after the shell files and before Grok Build, the window's overlay door
  (`yagura-daemon.exe --launch-overlay`, the `{daemon}` element of its `[command]`) with what the
  guest holds of Grok Build's settings, and starts Grok Build only with the overlay it printed, in
  `GROK_CONFIG`; a door that fails or prints nothing ends the window with one line saying so. The
  overlay keeps the window's credentials out of the shells Grok Build's tools run (see
  [Launch overlay](../profiles/README.md#launch-overlay)). A run that opens no window -- a
  registration, a listing -- starts Grok Build without the door.

A Windows path is not readable from the Linux CLI as it is, so each of the five profiles also has
a top-level `[host_paths]` table that converts every path the daemon hands the CLI with
`wsl -e wslpath -u` -- the bridge and the daemon a registration writes, and a settings file or
directory a launch injection names. Their own launch injections name no path, so opening one of
their windows runs no conversion -- except `wsl-copilot-cli`, whose hooks are a plugin directory
GitHub Copilot CLI takes only by its path, and `wsl-muse-code`, whose bridge and managed hooks
file Muse Code takes only by their paths; those are converted at each launch. See
[Host paths](../profiles/README.md#host-paths).

Because the shell is both a login and an interactive one, `~/.profile` and `~/.bashrc` run inside
the window's process tree before the CLI replaces bash, and that tree's environment already carries
the window's identity token (`YAGURA_STATUS_TOKEN`) when the profile keeps its credentials in the
environment (`wsl-codex-cli`, `wsl-grok-build`), so anything those files start can
identify itself as the window. The `wsl-claude-code`, `wsl-copilot-cli` and `wsl-muse-code` windows hold
none (see [Credentials](../profiles/README.md#credentials)). See "Who this identifies" under
[Requester Identification](mcp-setup.md#requester-identification) for why the token is on the whole
tree.

None of the five WSL profiles names a `[command]` key for macOS or Linux, so on either of those
the daemon leaves them out of every profile listing --
see profiles/README.md's schema section for the OS-specific `[command]` selection rule.

## The Folder a WSL Window Starts In

A window opened from one of the five WSL profiles without a folder starts in the distribution's
home (`~`), the way a WSL terminal does, not in the Windows folder the daemon would otherwise use:
the profile's `[host_paths] home` prints that home as Windows reaches it (`wslpath -w "$HOME"`, a
`\\wsl.localhost\<distribution>\...` path), and the window is opened there. That path is the
window's recorded folder, and the one its workspace room is resolved from like any other: the
repository it is in, or the folder itself (see [send_chat](mcp-setup.md#send_chat)). An approver
asked about the open is shown that folder. When the home cannot be had, the window opens in the
daemon's default folder and the daemon logs a warning. See
[Host paths](../profiles/README.md#host-paths).

A folder you choose is used as it is. Choosing your Windows home (`C:\Users\<you>`) for a WSL
window is a different thing from the Linux home: the CLI sees it as `/mnt/c/Users/<you>`, which is
not its home, so a CLI that reads a project's settings from the folder it starts in takes the
settings folders kept there for the Windows build as that project's. Grok Build is one: started in
`/mnt/c/Users/<you>`, it reads the Windows build's `.grok/config.toml` there as the project's, and
an MCP server that file names replaces one of the same name in its own Linux settings. Open a WSL
window without a folder, or in a project folder, rather than in your Windows home.

## Model Lists from the WSL Profiles

When the GUI's Models tab asks for the model list of `wsl-codex-cli`, the daemon runs the same
launch form, prefix included, with the profile's listing arguments after the CLI -- for example
`wsl -e bash -lic 'exec "$0" "$@"' codex debug models`. The list therefore comes from the Linux
build inside the distribution, found through the same `PATH` after the same
`~/.profile` and `~/.bashrc` have run. The listing is not a window, so it gets none of the variables
a window's child is given. A CLI missing from the distribution makes bash exit with status 127, which
reads as `codex was not found` (the CLI's name, not `wsl`). `wsl-claude-code`, `wsl-muse-code`
and `wsl-grok-build` carry a fixed list and start nothing, and `wsl-copilot-cli` carries none. See [profiles/README.md](../profiles/README.md#model-choices).

A distribution that is not running yet -- or a WSL that has not started since the host booted --
can make the first listing noticeably slower than later ones, because it waits for the distribution
to start and for the login and interactive shell files to run. The listing gets 20 seconds; past
that it is ended and the tab shows `did not finish within 20 seconds`. Once the distribution is up,
the tab's refresh button asks again.

## Environment Variables

A Linux process does not see a Windows variable unless `WSLENV` names it. On a Windows host the
daemon therefore gives every window's child a `WSLENV` that names the variables it injects:
`YAGURA_WINDOW_ID` and `YAGURA_STATUS_TOKEN`, `YAGURA_MCP_BRIDGE/p` when it found an MCP bridge,
and `YAGURA_INSTANCE` when the daemon itself runs under a named instance (the child inherits that
variable rather than being given it). These are added after whatever `WSLENV` already held -- the
launch profile's own `[env]` value, or else the daemon's -- without repeating a name already there.

A window whose profile hands its credentials over in a file (`wsl-claude-code`, `wsl-copilot-cli`,
`wsl-muse-code`) has neither `YAGURA_WINDOW_ID` nor `YAGURA_STATUS_TOKEN` set, and its `WSLENV` does not
name them. The file is read by the Windows `yagura-daemon.exe` and `yagura-mcp.exe`, which WSL runs
through interoperability, so its path stays a Windows path (with `/` separators) and is not converted.

The `/p` flag has WSL translate the bridge's Windows path at the boundary, so inside the
distribution `YAGURA_MCP_BRIDGE` reads as its `/mnt/c/...` form. yagura does no translation of its
own.

## MCP from a WSL Window

The bridge a WSL window's CLI starts is the Windows `yagura-mcp.exe`, run through WSL's
interoperability with Windows executables (enabled by default), at the path `YAGURA_MCP_BRIDGE`
names:

- `wsl-claude-code` passes Claude Code a `--mcp-config` JSON whose `command` is
  `${YAGURA_MCP_BRIDGE}`, which Claude Code expands from its own environment.
- `wsl-codex-cli` has Codex CLI start `bash -c 'exec "$YAGURA_MCP_BRIDGE"'`, because Codex CLI does
  not expand variables in a `-c` value, and forwards the channel variables to it with `env_vars`.
- `wsl-copilot-cli` passes GitHub Copilot CLI an `--additional-mcp-config` JSON whose `command` is
  `${YAGURA_MCP_BRIDGE}`, which Copilot expands from its own environment and whose server gets that
  whole environment, `WSLENV` included.
- `wsl-muse-code` passes Muse Code a `-c mcpServers.<name>=...` JSON whose `command` is the bridge's
  path as WSL sees it, and whose `env` names the channel variables the window has, `WSLENV`
  included, since Muse hands a server only the variables its definition names. See
  [Configuring Muse Code CLI](mcp-setup.md#configuring-muse-code-cli) for what this profile does
  not do.

The `wsl-claude-code`, `wsl-copilot-cli` and `wsl-muse-code` definitions also pass the bridge
`--credentials <path>`, the window's credentials file, instead of the two identity variables.

The bridge then reaches the daemon over the same Windows named pipe as any other window's bridge.

`wsl-grok-build` carries no `[mcp]` table: Grok Build has no launch argument that adds an MCP
server. Its row in Settings > Integrations registers the bridge inside the distribution by the
`/mnt/c/...` path `wslpath` gives (see
[Configuring Grok Build](mcp-setup.md#configuring-grok-build)). Grok Build hands an MCP server its
whole environment, `WSLENV` included, so the Windows bridge receives the window's variables with no
setting of its own.

## Delivery from a WSL Window

Chat, handoffs and notices reach a WSL window's CLI the way they reach any other window's: through the
window's inbox and the CLI's own hooks (see
[How the daemon delivers text](mcp-setup.md#how-the-daemon-delivers-text)). The WSL profiles' hooks call
`yagura-daemon.exe`, which a Linux shell finds through the Windows part of WSL's `PATH`; a
distribution with `appendWindowsPath = false` cannot find it, so its hooks do nothing and nothing is
delivered (see
[How a hook finds the daemon](../crates/yagura-daemon/detection/README.md#bundled-profiles)). The
`wsl-grok-build` hooks are the exception: they name the daemon's copy by the `/mnt/c/...` path
`wslpath` gives, so they do not depend on `PATH`.

**The wake file.** A `wsl-claude-code` window whose wait is gone is asked to register it again
through a wake file (see [How the daemon delivers text](mcp-setup.md#how-the-daemon-delivers-text)).
Claude Code inside the distribution watches it by its Linux path, so the file lives in the guest's
own state folder, under the instance's folder there:
`$XDG_STATE_HOME/yagura[-<instance>]/host-wake/<number>/yagura_wake` (`~/.local/state/...` when
`XDG_STATE_HOME` is unset), which the profile's `[host_paths] state_base` prints together with the
`\\wsl.localhost\...` path the daemon writes it by (see
[Host paths](../profiles/README.md#host-paths)). The numbered folders are used again, lowest free
number first, and never removed: there are never more of them than WSL Claude Code windows open at
once. yagura never wakes a distribution that has stopped: it reaches the guest's files only when a
window opens (the `state_base` run, and making the window's numbered folder before its CLI starts,
since Claude Code does not see a file appear in a folder that was missing when it began watching)
and when it writes the file of a window that is running. A window whose `state_base` or folder did
not finish within the daemon's listing time limit (10 seconds; a distribution that is still starting
can take that long) opens without a wake file, and what is sent to it while its CLI waits arrives at
its next turn, as the reply's `note` says.

`wsl-codex-cli` wakes a waiting Codex CLI with the profile's own launch form followed by
`queue --thread <session> --message <text>`, the same way a model listing runs (see
[Model Lists from the WSL Profiles](#model-lists-from-the-wsl-profiles)), so the Linux build inside
the distribution receives the text unchanged. The program the daemon starts is `wsl.exe`, not an npm
`.cmd` shim, so this profile needs no standalone build of Codex CLI the way the Windows-native
`codex-cli` profile does (see [Delivery](../profiles/README.md#delivery)).

## The Working Directory

The working directory field (`Working directory`, marked `optional`) is resolved as a **Windows**
path before the process starts, and that directory is handed to `wsl.exe`. yagura performs no path
translation of its own — the `/mnt/c/...` form below is what the shell inside the distribution
reports for that same directory.

This was measured with `pwd` in the pane:

| Working directory field        | `pwd` inside the distribution       |
| ------------------------------ | ----------------------------------- |
| `C:\Users\you\projects\yagura` | `/mnt/c/Users/you/projects/yagura/` |

(The run used a real user directory; the path above keeps its shape with the account and project
folder renamed.)

A Windows path entered in the field therefore arrives as its `/mnt/c/...` equivalent and works as
intended.

**Blank does not mean "the project root."** It means your Windows home directory, the same directory
any window opened without a working directory starts in, and it is handed to `wsl.exe` the same way.
Fill the field in when the starting directory matters.

## Limits Observed

- **Processes inside the distribution have no Windows PID.** yagura terminates a window's process
  tree through a per-window Windows job object, and only `wsl.exe` and its Windows-side helper
  processes are ever assigned to it. A `bash` — or anything it starts — running inside the
  distribution is not a Windows process, is not in that job object, and is not visible to yagura's
  process enumeration.
- **Close and interrupt were not verified for WSL windows.** Because of the point above, the
  manual verification deliberately excluded WSL windows from the process-tree termination and
  no-collateral-damage checks. What happens to processes inside the distribution when such a window
  is closed or interrupted is unmeasured.
- **Status detection was not exercised** on a WSL window; the verification only covered opening the
  window, the prompt, and the working directory.
- **The MCP route from a WSL window was measured on one setup.** A variable named in `WSLENV`
  reached the distribution, `/p` turned a `C:\...` path into its `/mnt/c/...` form, and a Windows
  program started from inside the distribution through interoperability received the variables.
  With the Linux build of Codex CLI installed through nvm, a window opened from `wsl-codex-cli`
  identified itself to the daemon and called a yagura tool; a window opened from `wsl-claude-code`
  identified itself as well.
- **WSL windows are not kept apart from each other.** The daemon takes a window's hooks and bridge
  as the window's only when they run in its process tree, and a process inside WSL can place a
  Windows program in another WSL window's tree; see
  [Limits of the process tree check](mcp-setup.md#limits-of-the-process-tree-check).

## What Does Work Like Any Other Window

A WSL window is owned by the daemon, not by the GUI. In the verified run, quitting the GUI and
starting it again brought every window back — the two WSL windows included — with their panes still
answering input.
