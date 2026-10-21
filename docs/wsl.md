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
open it from the `WSL: Claude Code`, `WSL: Codex CLI` or `WSL: GitHub Copilot CLI` profile
instead (`WSL: Antigravity CLI`
opens a window the same way but needs its own manual MCP registration -- see
[MCP from a WSL Window](#mcp-from-a-wsl-window)).

## The Launch Form of the Bundled WSL Profiles

The four WSL profiles start their CLI with

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

A Windows path is not readable from the Linux CLI as it is, so each of the four profiles also has
a top-level `[host_paths]` table that converts every path the daemon hands the CLI with
`wsl -e wslpath -u` -- the bridge and the daemon a registration writes, and a settings file or
directory a launch injection names. Their own launch injections name no path, so opening one of
their windows runs no conversion -- except `wsl-copilot-cli`, whose hooks are a plugin directory
GitHub Copilot CLI takes only by its path, converted at each launch. See [Host paths](../profiles/README.md#host-paths).

Because the shell is both a login and an interactive one, `~/.profile` and `~/.bashrc` run inside
the window's process tree before the CLI replaces bash, and that tree's environment already carries
the window's identity token (`YAGURA_STATUS_TOKEN`), so anything those files start can identify
itself as the window. See "Who this identifies" under
[Requester Identification](mcp-setup.md#requester-identification) for why the token is on the whole
tree.

None of the four WSL profiles names a `[command]` key for macOS or Linux, so on either of those
the daemon leaves them out of every profile listing --
see profiles/README.md's schema section for the OS-specific `[command]` selection rule.

## Model Lists from the WSL Profiles

When the GUI's Models tab asks for the model list of `wsl-codex-cli` or `wsl-antigravity-cli`, the
daemon runs the same launch form, prefix included, with the profile's listing arguments after the
CLI -- for example `wsl -e bash -lic 'exec "$0" "$@"' codex debug models`. The list therefore comes
from the Linux build inside the distribution, found through the same `PATH` after the same
`~/.profile` and `~/.bashrc` have run. The listing is not a window, so it gets none of the variables
a window's child is given. A CLI missing from the distribution makes bash exit with status 127, which
reads as `codex was not found` (the CLI's name, not `wsl`). `wsl-claude-code` carries a fixed list
and starts nothing, and `wsl-copilot-cli` carries none. See [profiles/README.md](../profiles/README.md#model-choices).

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

The bridge then reaches the daemon over the same Windows named pipe as any other window's bridge.

`wsl-antigravity-cli` carries no `[mcp]` table -- Antigravity CLI (`agy`) reads MCP servers only
from its own global registration, the same as its Windows-native counterpart (see
[Configuring Antigravity CLI](mcp-setup.md#configuring-antigravity-cli)). Register it with
`agy mcp add yagura -- bash -c 'exec "$YAGURA_MCP_BRIDGE"'` inside the distribution; a WSL window
yagura opens carries the same `YAGURA_MCP_BRIDGE` (translated by `/p`, as above), so the variable
resolves there exactly as it does for the two profiles above.

## Delivery from a WSL Window

Chat, handoffs and notices reach a WSL window's CLI the way they reach any other window's: through the
window's inbox and the CLI's own hooks (see
[How the daemon delivers text](mcp-setup.md#how-the-daemon-delivers-text)). The WSL profiles' hooks call
`yagura-daemon.exe`, which a Linux shell finds through the Windows part of WSL's `PATH`; a
distribution with `appendWindowsPath = false` cannot find it, so its hooks do nothing and nothing is
delivered (see
[How a hook finds the daemon](../crates/yagura-daemon/detection/README.md#bundled-profiles)).

`wsl-codex-cli` wakes a waiting Codex CLI with the profile's own launch form followed by
`queue --thread <session> --message <text>`, the same way a model listing runs (see
[Model Lists from the WSL Profiles](#model-lists-from-the-wsl-profiles)), so the Linux build inside
the distribution receives the text unchanged. The program the daemon starts is `wsl.exe`, not an npm
`.cmd` shim, so this profile needs no standalone build of Codex CLI the way the Windows-native
`codex-cli` profile does (see [Delivery](../profiles/README.md#delivery)).

## Folder Trust Is Separate Inside the Distribution

Antigravity CLI keeps its own list of trusted working directories, and that list is not shared
between the Windows-native binary and the one running inside WSL: a directory already trusted on
the Windows side still shows `agy`'s "Do you trust the contents of this project?" confirmation the
first time it runs there from a `WSL: Antigravity CLI` window, independent of whether the same path
was ever confirmed outside the distribution.

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

## What Does Work Like Any Other Window

A WSL window is owned by the daemon, not by the GUI. In the verified run, quitting the GUI and
starting it again brought every window back — the two WSL windows included — with their panes still
answering input.
