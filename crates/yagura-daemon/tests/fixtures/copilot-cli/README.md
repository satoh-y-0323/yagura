# GitHub Copilot CLI screen fixtures

Thirty-five real-session captures, used by `tests/detector_copilot_cli.rs` to exercise
`crates/yagura-daemon/detection/copilot-cli.toml` against real screen content
instead of hand-written approximations.

Copilot draws its composer one of two ways. When the terminal answers its color
queries (OSC 10, 11 and 4), the composer is a band of half blocks (`╻▄▄▄`, rows
starting with `┃`, `╹▀▀▀`); when nothing answers, it sits between two rules
(`────`, `❯`, `────`). The daemon answers those queries from the moment a window
opens, with the colors the GUI last sent or the built-in dark ones, so a yagura
window gets the first drawing; the second was captured while the daemon answered
them only after a GUI view had sent the window its palette. The fixtures without
a suffix are the first; the `-no-palette` ones are the second.
The two dialogs (`trust`, `trust-wsl`, `approval-command`) read the same either
way, so they are captured once.

## Source

| fixture | cols | rows | expected | version |
|---|---|---|---|---|
| 100x30-idle-startup | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-idle-startup-wsl | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-done | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-waiting-input-draft | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-interrupted | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-running | 100 | 30 | Running | 1.0.101 |
| 100x30-running-esc-once | 100 | 30 | Running | 1.0.101 |
| 100x30-approval-command | 100 | 30 | WaitingApproval | 1.0.101 |
| 100x30-trust | 100 | 30 | WaitingApproval | 1.0.101 |
| 100x30-trust-wsl | 100 | 30 | WaitingApproval | 1.0.101 |
| 100x30-running-just-submitted | 100 | 30 | Running | 1.0.101 |
| 100x30-waiting-input-wrapped-draft | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-idle-startup-no-palette | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-idle-startup-wsl-no-palette | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-done-no-palette | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-waiting-input-draft-no-palette | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-interrupted-no-palette | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-running-no-palette | 100 | 30 | Running | 1.0.101 |
| 100x30-running-esc-once-no-palette | 100 | 30 | Running | 1.0.101 |
| 100x30-running-just-submitted-no-palette | 100 | 30 | Running | 1.0.101 |
| 100x30-waiting-input-wrapped-draft-no-palette | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-tab-sessions | 100 | 30 | Unknown | 1.0.101 |
| 100x30-tab-issues | 100 | 30 | Unknown | 1.0.101 |
| 100x30-tab-pull-requests | 100 | 30 | Unknown | 1.0.101 |
| 100x30-tab-gists | 100 | 30 | Unknown | 1.0.101 |
| 100x30-tab-back-to-current | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-sidebar-idle | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-sidebar-draft | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-sidebar-running-just-submitted | 100 | 30 | Running | 1.0.101 |
| 100x30-sidebar-running | 100 | 30 | Running | 1.0.101 |
| 100x30-sidebar-running-esc-once | 100 | 30 | Running | 1.0.101 |
| 100x30-sidebar-approval-command | 100 | 30 | WaitingApproval | 1.0.101 |
| 100x30-sidebar-interrupted | 100 | 30 | WaitingInput | 1.0.101 |
| 100x30-sidebar-focused | 100 | 30 | Unknown | 1.0.101 |
| 100x30-sidebar-closed | 100 | 30 | WaitingInput | 1.0.101 |

## Capture conditions

Captured 2026-10-21 against real GitHub Copilot CLI 1.0.101 sessions: the native
Windows binary for every fixture except the `-wsl` ones, which come from the WSL
binary started as the bundled `wsl-copilot-cli` profile starts it
(`wsl -e bash -lic 'exec "$0" "$@"' copilot`). A scratch tool (not part of this
repository) spawned the CLI through `PtySession::spawn_with_command_in_env`, the
function that starts every window's child, and recorded the raw PTY byte stream.
Each fixture is that recording replayed into a fresh `VtScreen` and read through
`VtScreen::bottom_lines(16)` at the point in the session named below, checked
against the live screen each time.

The child's environment is therefore the one a window's child gets: the
launching terminal's own variables removed and `TERM=xterm-256color`,
`COLORTERM=truecolor` set (`terminal_identity`). Inside WSL the CLI saw
`TERM=xterm-256color` and no `COLORTERM`, as `WSLENV` names neither. The CLI was
started with `--plugin-dir` pointing at the hooks the bundled profiles write, with
`PATH` led by a stand-in `yagura-daemon` that exits at once; the MCP options were
left out. For the fixtures without a suffix, the window's screen held the GUI's
fallback dark palette (`FALLBACK_THEME` in `gui/src/terminal/terminal-colors.ts`)
from right after the spawn, as a view attached at once leaves it; the
`-no-palette` ones had none.

| fixture | point in the session |
|---|---|
| 100x30-idle-startup | right after answering the folder trust prompt |
| 100x30-idle-startup-wsl | the same point, WSL binary |
| 100x30-done | after one completed turn |
| 100x30-waiting-input-draft | the composer holds an unsent draft |
| 100x30-interrupted | after interrupting a running turn with two Esc presses |
| 100x30-running | right after approving a shell command |
| 100x30-running-esc-once | while running, after a single Esc press |
| 100x30-approval-command | the shell command approval box |
| 100x30-trust | the folder trust prompt |
| 100x30-trust-wsl | the folder trust prompt, WSL binary |
| 100x30-running-just-submitted | a prompt sent a moment ago, the footer still reading `esc esc edit prompt` |
| 100x30-waiting-input-wrapped-draft | the composer holds a draft that wraps onto four rows, each starting with `┃ ` |

Each `-no-palette` fixture is taken at the same point as its counterpart; in
`100x30-waiting-input-wrapped-draft-no-palette` each continuation row is indented
by two spaces instead.

The `tab-` and `sidebar-` fixtures were captured on 2026-10-22 the same way, with
the GUI's palette, in a fresh shallow clone of a GitHub repository, as the Issues
and Pull requests tabs are left out of the cycle in a folder that is not one. In
each `sidebar-` fixture the session sidebar is open, so every row starts with the
sidebar up to its first `│`.

| fixture | point in the session |
|---|---|
| 100x30-tab-sessions | one `tab` press from Current: the Sessions tab |
| 100x30-tab-issues | the next `tab`: the Issues tab, with no open issues |
| 100x30-tab-pull-requests | the next `tab`: the Pull requests tab, with none open |
| 100x30-tab-gists | the next `tab`: the Gists tab, with no gists |
| 100x30-tab-back-to-current | the next `tab`, back on Current |
| 100x30-sidebar-idle | the sidebar opened with `←` over an empty composer |
| 100x30-sidebar-draft | the composer holds a draft that wraps onto two rows |
| 100x30-sidebar-running-just-submitted | that draft sent a moment ago |
| 100x30-sidebar-approval-command | the shell command approval box it led to |
| 100x30-sidebar-running | right after approving the command |
| 100x30-sidebar-running-esc-once | while running, after a single Esc press |
| 100x30-sidebar-interrupted | after the second Esc press |
| 100x30-sidebar-focused | a second `←`, moving the focus into the sidebar (the cursor hidden) |
| 100x30-sidebar-closed | the sidebar closed with `→`, back on the composer |

The wrapped-draft capture keeps text only, so the test puts the cursor at the end of
the draft, where the CLI leaves it, before judging it.

## Extraction rule

Each fixture is exactly 16 lines, LF-terminated, with no trailing whitespace
beyond what the terminal itself painted. When a screen's content does not reach
16 rows, the extraction yields the top rows of that screen rather than a padded
window, so some fixtures start with blank lines or end with a blank line.

## Redactions

Session-specific text that would tie a fixture to one operator's machine is
replaced with a fixed dummy; the replacement keeps the line's character width so
the right-aligned parts of the footer and the box borders stay in the same
columns.

- The working directory shown in the footer line above the composer
  (`~\AppData\Local\Temp\claude\...\workA` and the longer startup variants) is
  replaced with `~\work\project`.
- The same footer on the WSL binary (`/mnt/c/Users/<user>/.../workWA`) is
  replaced with `/mnt/c/Users/demo/work/project`.
- The directory path inside the trust prompt box (two wrapped lines) is replaced
  with `C:\Users\demo\work\project` on the first line and a blank box row on the
  second (`/mnt/c/Users/demo/work/project` in `100x30-trust-wsl`).
- The directory in the "don't ask again" option of `100x30-approval-command` is
  replaced with `~\work\project`.
- In the `tab-` and `sidebar-` fixtures the clone's directory in the footer and
  in the "don't ask again" option (`~\AppData\Local\Temp\...\workG`) is replaced
  with `~\work\project`.
- In `100x30-tab-sessions` the title of every earlier session in the list is
  replaced with `Earlier session`.

Nothing else in the captured text was altered.
