# Codex CLI screen fixtures

Twelve bottom-of-screen captures of real Codex CLI sessions, plus two synthetic
variants, used by `tests/detector_codex_cli.rs` to exercise
`crates/yagura-daemon/detection/codex-cli.toml` against real screen content instead of
hand-written approximations. Eleven captures come from a 0.152.1 session (composer,
work, and command-approval screens); the directory-trust capture comes from a 0.154.0
session. Each synthetic fixture is derived from a capture of the same version, so every
fixture's version is recorded per row below rather than for the set as a whole.

The set is not a complete screen inventory: it covers the screens whose verdict the
rules were derived from. Screens verified by their footer text alone, and screens that
are deliberately left `Unknown`, are listed under "Screen inventory".

## Source

| fixture | source snapshot | cols | rows | expected | version |
|---|---|---|---|---|---|
| 100x30-idle-startup | approval-rs/snap-035214-001.txt | 100 | 30 | WaitingInput | 0.152.1 |
| 100x30-running | approval-rs/snap-035551-004.txt | 100 | 30 | Running | 0.152.1 |
| 100x30-running-background | approval-rs/snap-035615-026.txt | 100 | 30 | Running | 0.152.1 |
| 100x30-approval | approval-rs/snap-035619-031.txt | 100 | 30 | WaitingApproval | 0.152.1 |
| 100x30-done | approval-rs/snap-035640-034.txt | 100 | 30 | WaitingInput | 0.152.1 |
| 100x30-fullauto-running | fullauto-rs/snap-040153-009.txt | 100 | 30 | Running | 0.152.1 |
| 100x30-fullauto-done | fullauto-rs/snap-040155-011.txt | 100 | 30 | WaitingInput | 0.152.1 |
| 80x24-idle-startup | approval-80x24/snap-042851-002.txt | 80 | 24 | WaitingInput | 0.152.1 |
| 80x24-running | approval-80x24/snap-043027-029.txt | 80 | 24 | Running | 0.152.1 |
| 80x24-approval | approval-80x24/snap-043028-030.txt | 80 | 24 | WaitingApproval | 0.152.1 |
| 80x24-done | approval-80x24/snap-043117-035.txt | 80 | 24 | WaitingInput | 0.152.1 |
| 100x30-trust | trust-codex/snap-085823-001.txt | 100 | 30 | WaitingApproval | 0.154.0 |
| 100x30-approval-with-working-synthetic | synthetic (see below) | 100 | 30 | WaitingApproval | 0.152.1 |
| 100x30-numbered-dialog-unknown-synthetic | synthetic (see below) | 100 | 30 | Unknown | 0.154.0 |

## Capture conditions

- 0.152.1: captured 2026-09-03, PTY geometries 100x30 and 80x24.
- 0.154.0: captured 2026-09-16, PTY geometry 100x30, in a directory the CLI had no
  trust record for, so its first screen is the directory-trust prompt. The prompt was
  left unanswered.

## Extraction rule

Each fixture is the engine's own `VtScreen::bottom_lines(16)` of the
captured screen -- the same arithmetic the detector applies to the live PTY
buffer, not a manual line count. Every fixture is exactly 16 lines,
LF-terminated, with no trailing whitespace on any line. When a screen's
content does not reach 16 rows, the extraction yields the top rows of that
screen rather than a padded window.

For `100x30-trust` and its synthetic sibling the deciding row is the footer (row 10 of
16), which is also the last row with content, so the match region -- the last
`bottom_line_count` rows counted from `max(last content row, 16)` -- covers it.

Path fragments in the footer and directory rows (`directory: ...`, the
bottom-right `~\...` segment, the trust prompt's `You are in ...` row) have been
replaced with the fixed dummy `~\work\project`; nothing else in the captured text was
altered.

## Synthetic fixtures

`100x30-approval-with-working-synthetic.txt` is derived from
`100x30-approval.txt` by replacing the line two rows above the trailing
`Press enter ...` line with `• Working (12s • esc to interrupt)`. It exercises
a screen where an approval dialog's own aggregate anchor is still present
while a `running`-shaped `• Working (...)` line also appears earlier in the
same bottom-16 window, so a priority ordering bug (`running` outranking
`waiting_approval`) shows up as a wrong verdict on this fixture specifically
rather than only in the fixture's own unmodified sibling.

`100x30-numbered-dialog-unknown-synthetic.txt` is derived from `100x30-trust.txt` by
replacing its only footer line, `  Press enter to continue`, with
`  Choose an option and confirm` (same line count, every other line byte-identical).
It stands for a dialog this definition does not know: its highlighted option row
`› 1. Yes, continue` still looks like a caret line, but no rule covers its footer, so
the verdict has to stay `Unknown` -- however long the screen goes unchanged.

## How a dialog is recognised

The `waiting_approval` rule matches the *footer* of a dialog, not its body. A dialog
that waits for the user to pick an option draws no composer and no work line of its
own, so the footer is the only stable evidence on the screen; dialog bodies vary per
feature while the footer is assembled from a small set of key hints. The rule is a
case-insensitive alternation of three shapes, each justified by real screens:

- `Press <key> ...` (`enter` / `space` / `a number`): the confirmation and multi-select
  dialogs, including the command-approval dialog (`Press enter to confirm or esc to
  cancel`) and the directory-trust dialog (`Press enter to continue`).
- `enter to submit`: the elicitation and question dialogs, whose footers list keys
  without a leading `Press`.
- `Use ↑/↓ to move,`: the two dialogs whose footer opens with the movement hint.

`^\s{0,4}` allows the leading indentation a view adds when it insets its footer.
Casing is not fixed by the CLI (the same hint is drawn `enter` in one screen and
`Enter` in another), hence the case-insensitive flag.

Screens that only *mention* one of those words in a different shape do not match:
`Press esc to cancel`, `Press the new key now. Esc cancels.`,
`Use tab / ↑ ↓ to move, enter to select, esc to close` (the AppLink view),
`enter to resume     esc to start new ...` (the resume picker) and
`enter create task    ctrl+j newline    esc tasks` (the agents view) are all left to
the other rules, and with no other rule matching they end up `Unknown`.

## Why a numbered option row is not the composer

Codex draws both the composer and a dialog's highlighted option row with the same
`› ` caret at column 0, and while a dialog is up the composer is not drawn at all. A
bare `^› ` therefore reads a dialog as a window ready for input, which is the one
mistake that matters: it would let the daemon deliver a message into a screen that is
waiting for a choice. The `waiting_input` rule consequently requires the text after
the caret to *not* look like a numbered option (`1. `, `12. `, ...), which leaves the
dialog to the footer rule or to `Unknown`.

The two defences are independent: a dialog whose footer is covered stays
`waiting_approval` even if the caret rule is widened, and a dialog whose footer is not
covered stays `Unknown` even without the footer rule. The cost is a known false
negative in the safe direction: while the user types a line that itself begins with
`1. `, the composer is not recognised and the window reads `Unknown`.

`/agents` remains a residual: its `New task` field is drawn exactly like the composer,
so no structural rule can tell it apart, and it stays `waiting_input`.

## Screen inventory

How each screen of the 0.154.0 inventory is covered. `evidence` says how the claim was
checked: `captured` = a fixture in this directory, `string table` = the footer's
verbatim text asserted against the rule in `tests/detector_codex_cli.rs`,
`not captured` = neither.

| screen | class | rule or `unknown` | evidence | version |
|---|---|---|---|---|
| directory trust | selection | waiting_approval | captured | 0.154.0 |
| command / patch / permission approval | approval | waiting_approval | captured | 0.152.1 |
| login method, Bedrock, update, cwd, import source | selection | waiting_approval | string table | 0.154.0 |
| OSS provider | selection | waiting_approval | string table | 0.154.0 |
| hooks trust, generic lists | selection | waiting_approval | string table | 0.154.0 |
| unarchive | selection | waiting_approval | string table | 0.154.0 |
| feedback | input + selection | waiting_approval | string table | 0.154.0 |
| experimental features | selection | waiting_approval | string table | 0.154.0 |
| skills | selection | waiting_approval | string table | 0.154.0 |
| memories | selection | waiting_approval | string table | 0.154.0 |
| usage-limit banner | selection | waiting_approval | string table | 0.154.0 |
| request_user_input | selection | waiting_approval | string table | 0.154.0 |
| MCP elicitation | approval | waiting_approval | string table | 0.154.0 |
| model migration | selection | waiting_approval | string table | 0.154.0 |
| import review | selection | waiting_approval | string table | 0.154.0 |
| hooks browser, statusline setup, title setup | selection | waiting_approval (expected) | not captured; footer text not fixed by the source | 0.154.0 |
| resume picker, AppLink, `/agents` | selection | unknown (resume picker, AppLink); waiting_input (`/agents`, residual) | string table | 0.154.0 |
| async questions (expanded), other-thread approval notice | composer position / coexisting | running or unknown (async questions); waiting_input (other-thread notice) | not captured | 0.154.0 |
