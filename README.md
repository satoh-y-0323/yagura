# yagura

**Run agent CLIs from different vendors side by side, and let them work together.**

yagura is a desktop app that runs any command -- Claude Code, Codex CLI, GitHub Copilot CLI,
Grok Build, Muse Code, or a plain shell -- in its own window, tells you at a glance which window
is working and which one is waiting for you, and gives the agents in those windows an MCP server to
see and talk to each other.
You stay in control: anything one agent does to another window waits for your approval unless you
allow it.

## Features

**Many windows, one screen**

- Every window is a real terminal running the command you choose. Work in Focus, where the current
  window fills the work area, or in Canvas, where windows float: drag, resize, minimize, maximize
  or tile them in a grid. yagura remembers which of the two you use.
- The sidebar groups windows by the repository or directory they run in, and shows each one's
  status, vendor and working directory.
- Windows keep running in a background daemon, so closing the GUI does not stop your agents. Next
  time, yagura offers to bring your last layout back.

**Know who needs you**

- Each window is `running`, `waiting_input`, `waiting_approval`, `interrupted`, `idle`, `exited` or
  `unknown`, read from the screen by detection definitions for each CLI.
- A badge counts the windows waiting for you; one key jumps to the next.
- When yagura cannot tell, it says `unknown` and why, instead of guessing -- and it will not push
  messages to or interrupt a window it cannot read.

**Agents that work together**

- `yagura-mcp` gives an agent tools to list, open, read, message, interrupt and close windows.
  Windows opened from the bundled Claude Code, Codex CLI and GitHub Copilot CLI profiles, and from
  the Muse Code profiles on macOS, Linux and WSL, are connected automatically; register yagura in
  Settings > Integrations for Grok Build and, on Windows, Muse Code.
- **Handoff**: pass a reference and a note to another window -- or to a new one, opened with the
  CLI you choose -- and learn whether its CLI took it.
- **Chat rooms**: agents and you post to each other with `@name`, `@parent` and `@user`. A room
  stops itself on loops or runaway turns and waits for you.
- Nothing is typed into a CLI's input line. Messages, handoffs, chat and notices wait in the
  window's inbox and reach the CLI through its own hooks and start-up request; the sender learns
  whether one was delivered or is still queued, and can take a queued one back. The GUI marks a
  window whose inbox holds waiting messages with an envelope and their number, on its sidebar row
  and in its header. See
  [how delivery works](docs/mcp-setup.md#how-the-daemon-delivers-text).
- **Interrupting**: your own interrupt presses the CLI's key. An agent's stops the CLI at its next
  tool call through its hooks, without touching your draft; Codex CLI refuses an agent's
  interrupt. See [interrupt_window](docs/mcp-setup.md#interrupt_window).
- **Plans**: an agent hands yagura a plan of tasks with dependencies. yagura works out what can
  run in parallel, refuses tasks that would write the same files at the same time, and opens each
  task's window with the CLI, model and effort you assign.
- **Reports**: a window tells the one that opened it how its work ended.
- **Tickets**: each repository has a ticket board that agents keep through MCP and you read in the
  app. The states and the rules are yours, in `tickets.toml`; yagura enforces only the fields a
  state requires.

**You decide**

- Closing, interrupting or messaging another window waits for your decision in an approval
  dialog. Turn auto-approval on per tool in Settings > Approvals.
- New plans wait for your approval before any task window opens.
- yagura never touches a CLI's credentials. Each CLI signs in the way it always does.

**Make it yours**

- Launch profiles save a command, working directory, environment and model aliases per CLI.
- Dark, Light and System themes, or your own; per window or per vendor.
- Rebindable keyboard shortcuts for moving between windows and panels.

## Install

| Platform | How |
|---|---|
| Windows 11 | Download the installer from the [Releases](../../releases) page and run it. It installs for your user only (no administrator rights). It is not code-signed yet, so SmartScreen warns about an unrecognized publisher: choose **More info** > **Run anyway**. |
| macOS (Apple silicon) | Download the `.dmg` from the [Releases](../../releases) page, open it and drag **yagura** into **Applications**. The app is signed ad-hoc but not notarized, so macOS blocks the first launch: choose **Done**, then **Open Anyway** in **System Settings > Privacy & Security**. See [docs/macos-signing.md](docs/macos-signing.md). |
| Linux (Debian, Ubuntu) | Download the `.deb` from the [Releases](../../releases) page and install it with `sudo apt install ./yagura_<version>_amd64.deb`. |
| Other systems, Intel Macs | Build from source -- see [docs/building.md](docs/building.md). |

yagura supports the operating system releases their vendors still update: macOS 15 (Sequoia) or later,
and on Linux WebKitGTK 2.50 or later, as Ubuntu 22.04 and later ship it. A release that its vendor
stops updating is dropped in the next yagura release. Keep the system and its web engine (Safari on
macOS, WebKitGTK on Linux) up to date: yagura draws its window with them.

You also need the agent CLIs you want to run (Claude Code, Codex CLI, GitHub Copilot CLI, Grok
Build, Muse Code), installed and signed in as usual. yagura does not install or sign in to
them for you. On Windows, install Codex CLI's standalone build rather than the npm package: yagura
wakes a waiting Codex CLI with a command that the npm package's `.cmd` launcher cannot pass text
through, and lists the Codex CLI profile as unavailable until the standalone build is first on
`PATH` (see
[profiles/README.md](profiles/README.md#delivery)).

## Quick Start

1. Start yagura. The daemon starts with it.
2. Click **+** (Add a window) at the top of the sidebar, pick a launch profile -- for example
   **Claude Code** -- and a working directory.
3. Open a second window with another CLI, such as **Codex CLI**, in the same directory.
4. Ask the agent in the first window to work with the second one, for example:

   > Ask the Codex CLI window to review the changes in this repository, and wait for its answer.

   The agent uses yagura's tools to send a handoff. yagura asks you to approve it (unless you have
   allowed it), and you can watch both windows work.

To use yagura's tools from a CLI session that yagura did not start, register the MCP bridge in
**Settings > Integrations**, or by hand as described in [docs/mcp-setup.md](docs/mcp-setup.md).

## Antigravity CLI Is Not Supported

yagura does not ship a profile, detection definition or Integrations entry for Antigravity CLI
(`agy`). Google's [Antigravity terms](https://antigravity.google/terms/) (section 6) treat
accessing the service through third-party software as a breach that can get the account
suspended or terminated, and its [FAQ](https://antigravity.google/docs/faq/) says that wrapping
`agy` inside third-party applications is not supported under consumer plans. A suspension can
also take away the other services you sign in to with the same Google account.

yagura does not stop any command from running, but we strongly recommend that you do not run
`agy` signed in with a consumer account inside yagura. Check the terms of your own plan with
Google. If you registered yagura with Antigravity CLI before, remove it as described in
[Removing yagura from Antigravity CLI](docs/mcp-setup.md#removing-yagura-from-antigravity-cli).

## Documentation

- [MCP setup and tool reference](docs/mcp-setup.md)
- [Plans](docs/plans.md)
- [Tickets](docs/tickets.md)
- [Launch profiles](profiles/README.md)
- [Detection definitions](crates/yagura-daemon/detection/README.md)
- [Adding a CLI](docs/adding-a-cli.md)
- [Themes](docs/themes.md)
- [Keyboard shortcuts](docs/keymap.md)
- [WSL](docs/wsl.md)
- [Running a second instance](docs/instances.md)
- [Connecting to the daemon](docs/client-config.md)
- [macOS signing](docs/macos-signing.md)
- [Building from source](docs/building.md)
- [Changelog](CHANGELOG.md)

## Security Notes

- The daemon accepts connections only from your own OS user, and only the yagura GUI can approve
  requests.
- Any MCP client connected to yagura can read the recent output of every window. Do not run
  terminals that show secrets as yagura windows.
- An agent can open a window and run any command in it as you, outside its own CLI's permission
  prompts. By default this needs no approval; switch **Open a window** off in Settings >
  Approvals to be asked each time.
- The shipped playbook also lets agents post in a chat room without asking; switch **Post in a
  chat room** off in Settings > Approvals to review each post.
- The approval dialog stops only what agents ask yagura to do over MCP. Inside its window, an
  agent or a shell runs as you, with your environment, like any terminal, and any program
  running as your OS user can get around yagura's approvals (see the residual risk under
  [Approver Registration](docs/mcp-setup.md#approver-registration)).
- With `YAGURA_LOG=debug`, the daemon log ([where it is](docs/mcp-setup.md#where-the-daemon-log-is))
  also records lines from each window's screen. Turn it off again once you are done diagnosing.
- yagura keeps each window's command line and working directory, the chat rooms, the plans and the
  tickets in its session store (`yagura.db`, see [Running a second instance](docs/instances.md) for where it
  lives) without encrypting them. Do not put secrets such as tokens or passwords in a window's
  command line; use the CLI's own login, or an environment variable set outside yagura, instead.
- Messages one agent sends another -- handoffs, chat and reports -- are data from another agent,
  not instructions from you. yagura delivers them as such.
- A window's id and token let its hooks and MCP bridge speak for it. In windows of the bundled
  Claude Code and GitHub Copilot CLI profiles, and of the WSL Muse Code profile, they reach only
  those, so a CLI that one of the window's tools starts cannot speak for the window. In a Codex CLI
  window the tools Codex runs get them empty. Other windows -- Grok Build, Muse Code outside WSL, a
  shell -- still carry them in their environment, where any program started inside
  can read them (see [profiles/README.md](profiles/README.md#credentials)).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
yagura by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
