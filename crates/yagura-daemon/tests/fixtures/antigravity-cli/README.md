# Antigravity CLI screen fixtures

Ten real-session captures plus one synthetic fixture, used by
`tests/detector_antigravity_cli.rs` to exercise
`crates/yagura-daemon/detection/antigravity-cli.toml` against real screen content
instead of hand-written approximations.

## Source

| fixture | cols | rows | expected | version |
|---|---|---|---|---|
| 100x30-idle-startup | 100 | 30 | WaitingInput | 1.2.11 |
| 100x30-waiting-input-draft | 100 | 30 | WaitingInput | 1.2.11 |
| 100x30-running | 100 | 30 | Running | 1.2.11 |
| 100x30-approval-command | 100 | 30 | WaitingApproval | 1.2.11 |
| 100x30-approval-mcp | 100 | 30 | WaitingApproval | 1.2.11 |
| 100x30-running-background | 100 | 30 | Running | 1.2.11 |
| 110x34-trust | 110 | 34 | WaitingApproval | 1.2.11 |
| 100x30-slash-menu | 100 | 30 | WaitingApproval | 1.2.11 |
| 100x30-interrupted | 100 | 30 | WaitingInput | 1.2.11 |
| 100x30-done | 100 | 30 | WaitingInput | 1.2.11 |
| 100x30-unknown-dialog-unknown-synthetic | 100 | 30 | Unknown | synthetic |

## Capture conditions

Captured 2026-09-26 on Windows against real Antigravity CLI 1.2.11 sessions
(both the native Windows binary and the WSL binary show the same screen
shapes; every capture here is from the Windows session), using a scratch tool
(not part of this repository) that spawns the CLI through the same
`PtySession` / `VtScreen` primitives this crate exercises in production, and
records the raw PTY byte stream. Each fixture here is that recording replayed
through `VtScreen::bottom_lines(16)` at the point in the session named by the
fixture -- the identical extraction the daemon runs against the live PTY
buffer, not a manual line count.

## Extraction rule

Each fixture is exactly 16 lines, LF-terminated, with no trailing whitespace
beyond what the terminal itself painted. When a screen's content does not
reach 16 rows, the extraction yields the top rows of that screen rather than
a padded window (see `100x30-idle-startup`, `100x30-waiting-input-draft` and
`100x30-slash-menu`, whose banner leaves several of the sixteen lines blank).

## Redactions

Two kinds of incidental, session-specific text have been replaced with a
fixed dummy so a fixture does not tie this test suite to one operator's
machine or account, the same way `tests/fixtures/codex-cli` replaces
directory-path fragments with `~\work\project`:

- The signed-in account's email address (shown in the startup banner) is
  replaced with `someone@example.com` everywhere it appeared.
- The scratch MCP server this session registered to observe a tool-call
  approval screen was named after the probing session; every occurrence of
  that name is replaced with `demoserver` (in the composer line asking for
  the call, the tool-approval dialog, and the `~/.gemini/.../mcp/...` path
  the CLI prints while resolving the tool's definition).
- `110x34-trust`'s workspace path (the scratch directory the trust prompt was
  captured against) is replaced with the dummy `C:\Users\demo\work\project`.

Nothing else in the captured text was altered.

## Synthetic fixture

`100x30-unknown-dialog-unknown-synthetic.txt` is hand-assembled, not derived
from a single capture: two real composer history lines (verbatim from the
`100x30-approval-mcp` session) are followed by a dialog this CLI's real
screens never showed the probe session -- a file-edit approval, named
"Files" the way the real MCP-tool dialog is named "MCP" -- ending in the
bare `esc to cancel` footer with no `↑/↓ Navigate` line and no spinner
anywhere in the 16-line window. It stands for a dialog this definition does
not know the shape of: it carries two `> `-prefixed history lines in the
match region, matching neither the `waiting_approval` rule (no `↑/↓
Navigate`) nor the `running` rule (no spinner, no `N task(s)`), yet still
carries the `unknown` rule's `esc to cancel` anchor above `waiting_input`'s
priority -- so the verdict must be `Unknown` rather than reading either
history line as an open composer.

## Why a background task's footer reads `running`

`100x30-running-background` shows an empty composer (`>` alone) with no
spinner glyph anywhere in the 16-line window -- the shape `waiting_input`
matches on every other fixture here. What keeps it out of `waiting_input` is
its footer, `... · low · 1 task(s) · /ta...`: a command the user already
approved is still executing in the background, and the CLI's own turn has
not finished evaluating it, so the daemon reads that footer as `running` the
same way `codex-cli.toml` reads its own background-terminal footer variant.

## Composer cursor position

`VtScreen::read_composer` scans upward from the terminal's own cursor row, so a caller
judging a captured screen has to put the cursor back where it really sat, not wherever
writing the fixture's text happens to leave it. That position is kept in
`tests/detector_antigravity_cli.rs` as two constants
(`IDLE_STARTUP_COMPOSER_CURSOR`, `WAITING_INPUT_DRAFT_COMPOSER_CURSOR`), not in the
fixture files themselves, so the fixture format stays the plain `bottom_lines(16)` text
described above.

The two values are recorded data: the real vt100 cursor position (row, col; 0-indexed
within the fixture's own 16-line window) the `redump` tool's own debug dump reported at
the moment each fixture was captured -- `cursor=(8,2)` for `100x30-idle-startup` (session
mark `dump:idle`) and `cursor=(8,14)` for `100x30-waiting-input-draft` (session mark
`dump:typed`). An empty composer's cursor sits at column 2, right after the `> ` marker. Every other fixture here is judged only through
`DetectionEngine`'s pattern rules, which never consult the cursor, so no other fixture
needs a recorded position.

## Screen inventory

Screens the probe sessions observed but that are not captured here as a
fixture, because their shape is covered by the sessions above or by string
matching alone rather than needing its own screen:

| screen | class | rule or `unknown` | evidence |
|---|---|---|---|
| `/mcp` server list (after the slash-menu selection is confirmed) | dialog | waiting_approval | not captured; footer is the same `↑/↓ Navigate` shape as `100x30-slash-menu` |
| folder-trust confirmation (WSL) | dialog | waiting_approval | not captured; same footer shape as `110x34-trust` |
| file-edit approval, URL-access approval | dialog | waiting_approval (expected) | not captured for real; `100x30-unknown-dialog-unknown-synthetic` stands in as the fail-safe case instead of asserting the real shape |
