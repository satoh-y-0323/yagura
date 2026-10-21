# GitHub Copilot CLI screen fixtures

Twelve real-session captures, used by `tests/detector_copilot_cli.rs` to exercise
`crates/yagura-daemon/detection/copilot-cli.toml` against real screen content
instead of hand-written approximations.

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

## Capture conditions

Captured 2026-10-21 against real GitHub Copilot CLI 1.0.101 sessions: the native
Windows binary for every fixture except the two `-wsl` ones, which come from the
WSL binary. A scratch tool (not part of this repository) spawned the CLI through
the same `PtySession` / `VtScreen` primitives this crate exercises in production
and recorded the raw PTY byte stream. Each fixture is that recording replayed
into a fresh `VtScreen` and read through `VtScreen::bottom_lines(16)` at the
point in the session named below, checked against the live screen each time.

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
| 100x30-waiting-input-wrapped-draft | the composer holds a draft that wraps onto four rows, each continuation row indented by two spaces |

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
- The same footer on the WSL binary (`/mnt/c/Users/<user>/.../workW`) is
  replaced with `/mnt/c/Users/demo/work/project`.
- The directory path inside the trust prompt box (two wrapped lines) is replaced
  with `C:\Users\demo\work\project` on the first line and a blank box row on the
  second (`/mnt/c/Users/demo/work/project` in `100x30-trust-wsl`).
- The directory in the "don't ask again" option of `100x30-approval-command` is
  replaced with `~\work\project`.

Nothing else in the captured text was altered.
