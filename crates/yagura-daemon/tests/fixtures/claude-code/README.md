# Claude Code screen fixtures

Twenty-one bottom-of-screen captures of Claude Code, used by
`tests/detector_claude_code.rs` to exercise `crates/yagura-daemon/detection/claude-code.toml`
against real screen content instead of hand-written approximations. Nineteen
are real captures and two are synthetic. The original sixteen span two PTY
geometries (100x30, 80x24) and two terminal-identity modes (`unicode`,
`ascii` -- the two glyph sets Claude Code's input prompt is drawn with)
across all four detector states; the five added afterwards are 100x30 only.
Capture versions differ per fixture -- see "Capture conditions" below.

## Source

| fixture | source run / snapshot | cols | rows | expected | L | P |
|---|---|---|---|---|---|---|
| 100x30-unicode-idle-startup | f-100x30-unicode/snap-194609-003.txt | 100 | 30 | WaitingInput | 9 | 7 |
| 100x30-unicode-running | f-100x30-unicode/snap-194624-005.txt | 100 | 30 | Running | 13 | 8, 11 |
| 100x30-unicode-done | f-100x30-unicode/snap-194626-007.txt | 100 | 30 | WaitingInput | 15 | 13 |
| 100x30-unicode-approval | f-100x30-unicode/snap-194652-013.txt | 100 | 30 | WaitingApproval | 16 | 10, 16 |
| 100x30-ascii-idle-startup | f-100x30-ascii/snap-194323-003.txt | 100 | 30 | WaitingInput | 9 | 7 |
| 100x30-ascii-running | f-100x30-ascii/snap-194339-005.txt | 100 | 30 | Running | 13 | 8, 11 |
| 100x30-ascii-done | f-100x30-ascii/snap-194341-007.txt | 100 | 30 | WaitingInput | 15 | 13 |
| 100x30-ascii-approval | f-100x30-ascii/snap-194406-012.txt | 100 | 30 | WaitingApproval | 16 | 10, 16 |
| 80x24-unicode-idle-startup | f-80x24-unicode/snap-194748-003.txt | 80 | 24 | WaitingInput | 9 | 7 |
| 80x24-unicode-running | f-80x24-unicode/snap-194803-005.txt | 80 | 24 | Running | 13 | 8, 11 |
| 80x24-unicode-done | f-80x24-unicode/snap-194806-008.txt | 80 | 24 | WaitingInput | 15 | 13 |
| 80x24-unicode-approval | f-80x24-unicode/snap-194831-013.txt | 80 | 24 | WaitingApproval | 16 | 10, 16 |
| 80x24-ascii-idle-startup | f-80x24-ascii/snap-194927-003.txt | 80 | 24 | WaitingInput | 9 | 7 |
| 80x24-ascii-running | f-80x24-ascii/snap-194943-005.txt | 80 | 24 | Running | 13 | 8, 11 |
| 80x24-ascii-done | f-80x24-ascii/snap-194945-007.txt | 80 | 24 | WaitingInput | 15 | 13 |
| 80x24-ascii-approval | f-80x24-ascii/snap-195011-013.txt | 80 | 24 | WaitingApproval | 16 | 10, 16 |
| 100x30-ascii-trust | v2-ascii/snap-193442-001.txt | 100 | 30 | WaitingApproval | 16 | 16 |
| 100x30-unicode-trust | trust-unicode/snap-081404-001.txt | 100 | 30 | WaitingApproval | 16 | 16 |
| 100x30-unicode-ask-user-question | askuserquestion-unicode5/snap-082330-010.txt | 100 | 30 | WaitingApproval | 16 | 16 |
| 100x30-unicode-approval-without-footer-synthetic | synthetic, derived from 100x30-unicode-approval | 100 | 30 | WaitingApproval | 14 | 10 |
| 100x30-unknown-dialog-synthetic | synthetic, authored | 100 | 30 | Unknown | 11 | none |

`L` is the fixture's own last line with content (1-indexed, out of the
fixture's 16 lines). `P` is the row (or rows, for `running` and, since the
`waiting_approval` pattern now carries two alternatives, for `approval`)
that carries the pattern a passing verdict depends on: the raw prompt line
for `idle-startup`/`done`, the spinner line and the raw prompt line for
`running`, the `Do you want to proceed?` line and the `Esc to cancel` footer
line for `approval` (both now anchor a match on their own), the `Esc to
cancel` footer line for `trust`/`ask-user-question`. The
`approval-without-footer-synthetic` fixture has only the first of those two
anchors left, by construction; `unknown-dialog-synthetic` has none.

## Extraction rule

Each fixture is cut with `target/debug/cut.exe <snapshot.txt> <cols> <rows>
16`, which replays the snapshot's screen rows through a `VtScreen` and emits
`VtScreen::bottom_lines(16)` -- the same arithmetic the detector applies to
the live PTY buffer, not a manual line count. Every fixture is exactly 16
lines, LF-terminated, with no trailing whitespace other than the retained
NBSP (see below). When a screen's content does not reach 16 rows, the
extraction yields the top rows of that screen rather than a padded window;
the four `idle-startup` and `running` fixtures here are examples of that
case.

`crates/yagura-daemon/detection/claude-code.toml` matches against `bottom_line_count = 12`
screen rows, not 16. The equivalent region inside a 16-line fixture is
computed with the same windowing arithmetic applied a second time, using the
fixture's own last content line `L`: rows `max(L, 12) - 11` through
`max(L, 12)` (1-indexed). This is the region a rule's anchor line `P` must
fall inside for the fixture to exercise the intended verdict; it is *not*
always the fixture's last 12 lines (a fixture whose content stops short of
row 12, like `idle-startup`, still has region `1..=12`).

The two synthetic fixtures were not run through `cut.exe` against a live
snapshot (there is none to cut): `approval-without-footer-synthetic` is a
byte-for-byte copy of `100x30-unicode-approval.txt` with its 16th line
replaced by an empty one, and `unknown-dialog-synthetic` is authored
directly as 16 lines. Both still satisfy the fixture's own invariants (16
lines, self-reproducing through `VtScreen::bottom_lines(16)`, verified with
`replay.exe`), which is what the `each_fixture_reproduces_itself_...` test
checks regardless of a fixture's origin.

## NBSP retention

The empty input box (`> ` / `❯ `) and the live prompt line both end the
prompt glyph with a no-break space (U+00A0), which the detector's
`waiting_input` pattern anchors on to distinguish the live prompt from a
plain `>`/`❯` echoed in scrollback history. Trailing-whitespace stripping in
this fixture set removes only U+0020; U+00A0 is preserved verbatim.

## What was replaced

Only two environment-dependent strings were substituted, both in the
welcome banner's directory line and plan line (present in the
`idle-startup`, `running`, and `done` fixtures; the `approval` fixtures have
scrolled the banner out of the captured window):

- the sandbox's absolute-turned-`~`-relative capture directory ->
  `~\work\project`
- the welcome banner's subscription-plan-tier suffix -> `· <plan>`

Nothing else in the captured text was altered. The model name in the
welcome banner (`Fable 5.1 with high effort`) is retained, matching the
`tests/fixtures/codex-cli` precedent of keeping model identifiers while
redacting account/path information.

`100x30-ascii-trust` and `100x30-unicode-trust` carry the same substitution,
applied to the one workspace-directory line their screen shows (`Accessing
workspace: <path>`) -> `~\work\project`; neither shows the welcome banner
(the trust dialog occupies the whole captured window), so the plan-line
substitution does not apply to them. `100x30-unicode-ask-user-question`
needed no substitution at all: its 16-line window starts below the banner
and contains no path or account-identifying text.

## Capture conditions

Captured against Claude Code `v2.1.260` on 2026-09-04, driven through
a developer-only capture script that is not part of this repository.
Each run strips the terminal-identity variables (`TERM`, `TERM_PROGRAM`,
`COLORTERM`, `LANG`, `WT_SESSION`, `VSCODE_GIT_IPC_HANDLE`) and the markers
an enclosing Claude Code session leaves behind (`CLAUDECODE`,
`CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID`,
`CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_MESSAGING_SOCKET`,
`CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_CODE_SSE_PORT`,
`CLAUDE_CODE_EXECPATH`) from its own environment, then supplies exactly one
of them back through `CAPTURE_EXTRA_ENV`: `TERM=xterm-256color` for
`unicode` runs, `TERM=xterm` for `ascii` runs. The captures run with cwd
outside the repository, under a per-user local app-data directory kept out
of version control, so no project or user `CLAUDE.md`/`AGENTS.md`
instruction file is on the child's read path. This applies to the original
sixteen fixtures and, since it is the same capture batch, to
`100x30-ascii-trust` as well.

The three fixtures added afterwards each have their own capture, run
directly through `target/debug/screen-capture.exe` (the same environment
stripping and `CAPTURE_EXTRA_ENV` regimen, invoked without the driver
script's fixed idle/running/done/approval sequence):

- `100x30-ascii-trust`: not a fresh run -- cut from the `v2.1.260` batch's
  own trust-dialog snapshot (`dumps/v2-ascii/snap-193442-001.txt`).
- `100x30-unicode-trust`: freshly captured against Claude Code `v2.1.273` on
  2026-09-16, in a previously-untrusted sandbox directory
  (`SCREEN_CAPTURE_SANDBOX` pointed at a directory never opened before),
  using the snapshot taken before any key was sent (the dialog's default
  focus, "No, exit", still selected). The dialog wording matches the
  `v2.1.260` capture verbatim aside from the prompt glyph and the directory
  name the two sandboxes show, which is what confirms the wording is
  unchanged across versions; `claude --version` on the capture host also
  reports `v2.1.273`.
- `100x30-unicode-ask-user-question`: freshly captured against Claude Code
  `v2.1.273` on 2026-09-16, in the existing (already-trusted) default
  sandbox, by writing an `AskUserQuestion`-triggering prompt ("Use the
  AskUserQuestion tool to ask me which color I prefer, with two options: red
  and blue. Do nothing else.") to the capture tool's `send.txt` and letting
  Claude Code render the resulting dialog.

`100x30-unicode-approval-without-footer-synthetic` and
`100x30-unknown-dialog-synthetic` are synthetic (see "What was replaced" and
"Screen inventory"); no capture version applies to either.

## Glyph-acceptance scope

Whether a fixture's verdict depends on which prompt glyph (`>` vs `❯`)
`waiting_input` accepts is limited to the eight `idle-startup`/`done`
fixtures (four ascii, four unicode) and the two `ascii` `running` fixtures
under the priority-mutated condition (`running`'s priority lowered below
`waiting_input`'s) exercised in `tests/detector_claude_code.rs`. Under the
definition's normal priority order, `running`'s own pattern (spinner line /
`esc to interrupt`) outranks `waiting_input`, so the `running` fixtures'
pass/fail does not turn on the glyph by itself -- they are included in glyph
scope only because the mutation test lowers `running` below `waiting_input`
and then relies on the raw prompt line's glyph matching. `approval`,
`trust`, and `ask-user-question` fixtures are glyph-independent:
`waiting_approval` matches on `Do you want to proceed?` and/or an `Esc to
cancel` footer, neither of which is the prompt glyph. The only visible
difference the glyph makes to any of those fixtures' own screens is in the
empty input-box line further up the window (`trust`/`ask-user-question` do
not show one at all), not in the anchor line(s) the rule reads.

## Screen inventory

Screens from a 58-row inventory of Claude Code's screens (kept outside this
repository) that this fixture set does or does not cover, beyond the four ordinary
detector states already listed in "Source" above.

| screen | class | rule or `unknown` | evidence | version |
|---|---|---|---|---|
| directory-trust confirmation | 2-choice confirmation | `waiting_approval` (`Esc to cancel`) | captured (`100x30-ascii-trust`, `100x30-unicode-trust`) | v2.1.260 / v2.1.273 |
| tool-use approval | selection list (numbered) | `waiting_approval` (`Do you want to` and `Esc to cancel`) | captured (existing `*-approval` fixtures) | v2.1.260 |
| AskUserQuestion | selection list (numbered) | `waiting_approval` (`Esc to cancel`) | captured (`100x30-unicode-ask-user-question`) | v2.1.273 |
| browser sign-in wait | input + inline wait | `waiting_approval` (`Esc to cancel`, partial-line match) | string table | v2.1.272 |
| session-goal proposal | unclear | `waiting_approval` (`Esc to cancel`, partial-line match) | string table | v2.1.272 |
| model picker | selection list (tabbed) | `waiting_approval` (`Esc to cancel`) | string table | v2.1.272 |
| login-complete notice | inline (Enter-wait, no composer) | `unknown` | string table | v2.1.272 |
| usage-limit inline notice | inline notice (composer present) | `unknown` (stays `waiting_input`) | string table | v2.1.272 |
| exit-twice notice | inline notice (composer present) | `unknown` (stays `waiting_input`) | string table | v2.1.272 |
| session-resume picker | selection list | `unknown` | string table | v2.1.272 |
| unconfirmed population: the remaining selection-list and confirmation screens (37 screens) | mixed selection lists / 2-choice confirmations, footer wording not confirmed live | `waiting_approval` if the rendered footer carries an `Esc`+`cancel` component, `unknown` otherwise | string table only (footer completion unconfirmed) | v2.1.272 |
| other group: title-only text or composer-adjacent inline text (11 screens) | title-only text or composer-adjacent inline text | `unknown`, or unaffected (stays `waiting_input`/`running`) | string table only | v2.1.272 |

`unknown-dialog-synthetic` represents the "unconfirmed population" row's
`unknown` branch directly: it is built from the initial theme-selection
screen's wording with its footer omitted, since that screen's own footer is
unconfirmed live (see the synthetic's own derivation below) rather than
copied from any single inventory row.

### Synthetic fixtures: derivation and differences

- `100x30-unicode-approval-without-footer-synthetic` is `100x30-unicode-approval.txt`
  with its 16th line (`Esc to cancel · Tab to amend`) replaced by an empty
  line; every other line, including the `Do you want to proceed?` line, is
  unchanged. It exercises the `waiting_approval` pattern's other
  alternative on its own.
- `100x30-unknown-dialog-synthetic` is authored, not captured: it uses the
  initial theme-selection wording from the inventory -- the prompt
  line, the seven theme options with a selection-list pointer, and the
  "To change this later, run /theme" hint -- but omits any footer line
  (that screen's own footer is unconfirmed live; the inventory records only that it
  sets `hideEscToCancel`, which does not by itself establish what, if
  anything, the footer says instead). It also omits a composer line, so it
  does not depend on the composer's absence coinciding with that screen's own; it
  represents "no rule matches" screens in general, not that screen specifically.

## Tooling

Captured with developer-only capture and cut tools that are not part of
this repository.
