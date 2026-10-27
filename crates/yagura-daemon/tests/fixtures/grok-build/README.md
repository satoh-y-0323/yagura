# Grok Build CLI screen fixtures

Real-session captures of Grok Build (`grok` 1.0.70), kept to exercise
`crates/yagura-daemon/detection/grok-build.toml` against real screen content
instead of hand-written approximations. The raw byte streams the composer
readout is checked against are in `composer/`.

**These are interim captures.** They were recorded with a standalone PTY probe
(`ptyprobe`, not part of this repository) started from Git Bash, not with a
window the daemon itself opened. A CLI can draw its composer differently under
the daemon's child environment (`TERM=xterm-256color`, `COLORTERM=truecolor`,
no terminal-name variables). They are to be re-captured from a daemon-opened
window (Windows and WSL) and replaced during the real-machine check of this
change (task T-1043, check M-D1); the expected verdicts below are what the
re-captured screens must keep.

Grok Build draws on the alternate screen. Its composer is a rounded box
(`╭──╮` / `│ > … │` / `╰── <model> (<effort>) ─╯`); the mark is `>` on Windows
and `❯` under WSL. The footer under the box lists the keys that apply, and is
what the detection rules read.

## Source

All captures: Grok Build 1.0.70, 100 columns x 30 rows. The `.txt` files are the
bottom 16 lines (`VtScreen::bottom_lines(16)`, the extraction rule
`detector_muse_code.rs` uses) of the recorded byte stream replayed into a fresh
`VtScreen(100, 30)`. The files under `composer/` are the recorded byte streams
themselves.

| fixture | platform | expected | point in the session |
|---|---|---|---|
| 100x30-idle-startup | Windows | WaitingInput | the empty composer right after start (PP-1/PP-4 `idle`) |
| 100x30-draft | Windows | WaitingInput | a two-row draft typed and not sent (PP-4 `draft`) |
| 100x30-running | Windows | Running | 4.5 seconds into a model turn; footer shows `Ctrl+c:cancel` (PP-4 `sent4500`) |
| 100x30-after-turn | Windows | WaitingInput | after the turn ended; `Ctrl+c:cancel` gone (PP-4 `t95`) |
| 100x30-approval | Windows | WaitingApproval | a permission box for `rm`; the box's own footer `1/5:select` (PP-5 `approval`) |
| 100x30-approval-history-focus | Windows | WaitingApproval | Esc after the permission box: the box stays, focus moved to the history, footer `Tab/Space:permission` and still `Ctrl+c:cancel` (PP-5 `afteresc`) |
| 100x30-interrupted-history-focus | Windows | Unknown | after Ctrl+C cancelled the turn: the history holds the focus, footer `Space:prompt` (PP-5 `ctrlc`) |
| 100x30-trust | Windows | WaitingApproval | the folder trust prompt; only its last line, `Grok Build  1.0.70 [stable]`, is within the bottom 16 lines (M2 `l15`) |
| 100x30-wsl-idle | WSL | WaitingInput | the empty composer after start, mark `❯` (PP-10 B `l10`) |
| 100x30-wsl-running | WSL | Running | 4 seconds into a turn, mark `❯` (PP-10 A `l4`) |
| 100x30-wsl-after-turn | WSL | WaitingInput | after the turn ended, mark `❯` (PP-10 A `l30`) |

| composer fixture | cols x rows | expected | screen |
|---|---|---|---|
| composer/100x30-empty.bin | 100x30 | Empty | Windows, the empty composer after start (the `idle` capture) |
| composer/100x30-draft.bin | 100x30 | Body (the typed prompt) | Windows, a draft of one wrapped line typed and not sent |
| composer/100x30-wsl-empty.bin | 100x30 | Empty | WSL, the empty composer after start, mark `❯` |

## Capture conditions

Captured 2026-10-28. Windows: `grok.exe` started directly (no shim), in a
scratch git repository, with `GROK_FOLDER_TRUST=0` for the turn captures. WSL:
`wsl --cd <folder> -e bash -lic 'exec "$0" "$@"' grok`, the form the bundled
`wsl-grok-build` profile launches. Nothing answered the CLI's color queries.
The first-run "Help improve Grok" consent notice was left unanswered on screen
in every capture; it does not take input. What decides `>` versus `❯` is not
known (the WSL captures show `❯`, the Windows ones `>`).

## Redactions

The working folder shown in the top bar of the byte streams under `composer/`
(abbreviated to initials) is replaced with `~/work/project`, padded with spaces
to the same byte length. No other byte was changed. The `.txt` files hold none
of it.
