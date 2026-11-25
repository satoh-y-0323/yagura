# Grok Build CLI screen fixtures

Real-session captures of Grok Build (`grok` 1.0.70 and 1.0.92), kept to exercise
`crates/yagura-daemon/detection/grok-build.toml` against real screen content
instead of hand-written approximations.

Grok Build draws on the alternate screen. Its composer is a box with the model
on its lower rule, and the footer under the box lists the keys that apply; the
footer and the lower rule are what the detection rules read. Its corners are
rounded (`╭──╮` / `│ ❯ … │` / `╰── <model> (<effort>) ─╯`) or square
(`┌──┐` / `│ > … │` / `└── <model> (<effort>) ─┘`). 1.0.70 on Windows drew the
rounded box with the mark `>`.

On Windows, 1.0.89 and later draw the square form unless they recognise the
terminal from its variables (`WT_SESSION`, or a `TERM_PROGRAM` they know, such
as `vscode`); a name they do not know, such as `TERM_PROGRAM=yagura`, keeps the
square form. The daemon takes the launching terminal's variables away from a
window's child, so every window it opens on Windows shows the square form
(1.0.89 and 1.0.92, 2026-11-25). Under WSL, Linux and macOS, 1.0.92 draws the
rounded box (seen on the same day).

## Source

The `100x30-*` captures: Grok Build 1.0.70, 100 columns x 30 rows, recorded
with a standalone PTY probe started from Git Bash, not from a window the daemon
opened. The `.txt` files are the bottom 16 lines (`VtScreen::bottom_lines(16)`,
the extraction rule `detector_muse_code.rs` uses) of the recorded byte stream
replayed into a fresh `VtScreen(100, 30)`.

| fixture | platform | expected | point in the session |
|---|---|---|---|
| 100x30-draft | Windows | WaitingInput | a two-row draft typed and not sent (PP-4 `draft`) |
| 100x30-approval-history-focus | Windows | WaitingApproval | Esc after the permission box: the box stays, focus moved to the history, footer `Tab/Space:permission` and still `Ctrl+c:cancel` (PP-5 `afteresc`) |
| 100x30-interrupted-history-focus | Windows | Unknown | after Ctrl+C cancelled the turn: the history holds the focus, footer `Space:prompt` (PP-5 `ctrlc`) |
| 100x30-wsl-idle | WSL | WaitingInput | the empty composer after start, mark `❯` (PP-10 B `l10`) |
| 100x30-wsl-running | WSL | Running | 4 seconds into a turn, mark `❯` (PP-10 A `l4`) |
| 100x30-wsl-after-turn | WSL | WaitingInput | after the turn ended, mark `❯` (PP-10 A `l30`) |

The folder trust prompt has no `100x30-*` capture: the bottom 16 lines of it
hold only the version line, not the question the definition reads, so such a
cut does not stand for the screen.

| daemon fixture | platform | cols x rows | expected | screen |
|---|---|---|---|---|
| daemon-1092-idle | Windows | 80x24 | WaitingInput | the start screen with the empty square composer; its last line is the version line, `Grok Build  1.0.92 [stable]` |
| daemon-1092-running | Windows | 80x24 | Running | a model turn under way; footer shows `Ctrl+c:cancel` |
| daemon-1092-after-turn | Windows | 80x24 | WaitingInput | after the turn ended; `Ctrl+c:cancel` gone |
| daemon-1092-approval | Windows | 80x24 | WaitingApproval | a permission box for `del a.txt`; the box's own footer `1/5:select` |
| daemon-1092-trust-screen | Windows | 80x24 | WaitingApproval | the folder trust prompt in a folder not yet trusted, every row of the screen |

The `daemon-*` files are the whole text `read_output` returned from a window
the daemon opened at its initial 80 x 24 (Grok Build 1.0.92, Windows,
2026-11-25). The turn and approval captures were taken in a scratch git
repository with `GROK_FOLDER_TRUST=0`, the turn started by the prompt given on
the command line.

## Capture conditions

The `100x30-*` captures, 2026-10-28. Windows: `grok.exe` started directly (no shim), in a
scratch git repository, with `GROK_FOLDER_TRUST=0` for the turn captures. WSL:
`wsl --cd <folder> -e bash -lic 'exec "$0" "$@"' grok`, the form the bundled
`wsl-grok-build` profile launches. Nothing answered the CLI's color queries.
The first-run "Help improve Grok" consent notice was left unanswered on screen
in every capture; it does not take input.
