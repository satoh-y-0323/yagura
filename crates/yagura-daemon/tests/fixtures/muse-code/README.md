# Muse Code CLI screen fixtures

Nineteen real-session captures of Muse Code, kept to exercise
`crates/yagura-daemon/detection/muse-code.toml` against real screen content
instead of hand-written approximations.

Muse Code draws on the normal screen (no alternate screen) and puts its composer,
a `❯ ` row, between two rules: on Windows the upper rule reads
`── Voice input (Alt+V to start) ──`, under WSL it is a plain `────` rule. The
footer below the lower rule reads `<model> · <effort> · <folder> · <approval>`
(`echo · <folder> · <approval>` with `--provider echo`).

## Source

| fixture | cols | rows | expected | version |
|---|---|---|---|---|
| 100x30-trust | 100 | 30 | WaitingApproval | 1.4.12 |
| 100x30-wsl-trust | 100 | 30 | WaitingApproval | 1.4.12 |
| 100x30-idle-startup | 100 | 30 | WaitingInput | 1.4.12 |
| 100x30-idle-model1 | 100 | 30 | WaitingInput | 1.4.12 |
| 100x30-paste-draft | 100 | 30 | WaitingInput | 1.4.12 |
| 100x30-draft-newline | 100 | 30 | WaitingInput | 1.4.12 |
| 100x30-after-enter2 | 100 | 30 | Running | 1.4.12 |
| 100x30-done1b | 100 | 30 | Running | 1.4.12 |
| 100x30-after-approve | 100 | 30 | Running | 1.4.12 |
| 100x30-approval | 100 | 30 | WaitingApproval | 1.4.12 |
| 100x30-mcp-approval | 100 | 30 | WaitingApproval | 1.4.12 |
| 100x30-settled1 | 100 | 30 | WaitingInput | 1.4.12 |
| 100x30-interrupted | 100 | 30 | WaitingInput | 1.4.12 |
| 100x30-mcp-long-done | 100 | 30 | Running | 1.4.12 |
| 100x30-echo-wake-async | 100 | 30 | WaitingInput | 1.4.12 |
| 100x30-wsl-idle | 100 | 30 | WaitingInput | 1.4.12 |
| 100x30-wsl-done | 100 | 30 | WaitingInput | 1.4.12 |
| 88x30-v1420-running-tool-output | 88 | 30 | Running | 1.4.20 |
| 88x30-v1420-done-after-tool | 88 | 30 | WaitingInput | 1.4.20 |

## Capture conditions

Captured 2026-10-23 against Muse Code 1.4.12 (`1.4.12-R6909.1`). A scratch tool
(not part of this repository) spawned the CLI through
`PtySession::spawn_with_command_in_env` with `DaemonConfig::default()`, in a
100x30 PTY, and recorded the raw PTY byte stream. Each fixture is that recording
replayed into a fresh `VtScreen(100, 30)` and read through
`VtScreen::bottom_lines(16)` at the point in the session named below.

- Windows native: either through `muse.cmd`, resolved the way the daemon resolves
  a profile's command (`resolve_program` → `into_spawn_argv`, which starts
  `cmd.exe /d /c ...\muse.cmd`), or by starting the CLI's own executable
  directly (needed to pass `-c` values the `muse.cmd` path refuses).
- WSL: `wsl -e bash -lic 'exec "$0" "$@"' muse`, as a window's command is
  started under WSL.
- Nothing answered the CLI's color queries.
- Every fixture except `100x30-trust` and `100x30-wsl-trust` was started with
  `--trust-workspace`, which trusts the folder for that run without saving it.
- Fixtures named `echo` used `--provider echo`, which answers without a model.

| fixture | launch | point in the session |
|---|---|---|
| 100x30-trust | Windows, `muse.cmd`, `--provider echo`, untrusted folder | the folder trust prompt right after start |
| 100x30-wsl-trust | WSL, `--provider echo`, untrusted folder | the folder trust prompt right after start |
| 100x30-idle-startup | Windows, the executable directly, `--approval-judge off` and per-run hooks (`Launch overrides`) | the empty composer after start |
| 100x30-draft-newline | the same window | a draft whose Enter, sent about 0.1 s after the text appeared, added a blank row instead of sending |
| 100x30-after-enter2 | the same window | the turn sent by a second Enter, `◈ Thinking (… · esc to interrupt)` |
| 100x30-done1b | the same window | later in that turn, `◇ Double checking (… · esc to interrupt)` |
| 100x30-settled1 | the same window | the empty composer after that turn |
| 100x30-paste-draft | the same window | a draft written as a bracketed paste, not yet sent |
| 100x30-interrupted | the same window | after one Esc interrupted a running shell command; the composer shows `Start a message with ! to run a shell command yourself` |
| 100x30-approval | the same window | the shell command approval box, with `◆ Calling tools (… · esc to interrupt)` above it |
| 100x30-after-approve | the same window | right after approving that command with `y`, `◇ Running command · … (… · esc to interrupt)` |
| 100x30-idle-model1 | Windows, `muse.cmd`, `--model muse-spark-1.3 --reasoning-effort low` | the empty composer after start |
| 100x30-mcp-approval | Windows, the executable directly, `--approval-judge off`, `-c reasoning_effort="low"` and a per-run MCP server | the MCP tool approval box (`Would you like to allow this tool action?`) |
| 100x30-mcp-long-done | Windows, launch not recorded (the footer reads `Launch overrides`) | after a 200-second MCP tool call answered and the turn ended; `◇ Double checking (… · esc to interrupt)` still above the empty composer |
| 100x30-echo-wake-async | Windows, `--provider echo`, a managed hook file holding an `asyncRewake` Stop hook; launch otherwise not recorded | after one turn, with the hook warning at the top and `Type @ to search and insert workspace file paths` in the composer |
| 100x30-wsl-idle | WSL, `--provider echo`, per-run managed hooks | the empty composer after start |
| 100x30-wsl-done | the same window | after one turn |

`88x30-v1420-running-tool-output` was captured on 2026-11-16 against Muse Code 1.4.20
(`muse-code` profile, Windows) in a yagura window 88 columns wide (a test instance of
the GUI and daemon); the row count of 30 is the fixture's frame and was not measured.
The daemon ran with debug logging, so every evaluation recorded its
`detector match window` lines, the bottom eight rows the rules read. The point is a
shell command (`ping -n 41 127.0.0.1`) running after it printed several rows: those
rows push the turn's heading, `◆ Running command · … running (0s · esc to interrupt)`,
above the eight rows, and the last of them ends with `(ctrl+b to send to background)`.
The bottom eight rows are the recorded lines, verbatim, trailing spaces included. The
eight rows above them are put together from the lines recorded a moment earlier in
the same turn (the heading as last recorded, at 0s, and the first output row) and from
`read_output` with 16 lines (the version banner). The CLI printed the command's cp932
output as if it were UTF-8, so the output rows hold replacement characters; they are
kept as the window showed them.

`88x30-v1420-done-after-tool` is the same window after that turn ended, with
`◆ Ran command · … · ✓ · 42s · ctrl+o` above `◆ done`. The bottom eight rows are the
recorded lines, verbatim; the eight rows above them are `read_output` with 16 lines
at the end of the scene. The recorded composer row reads `❯ `, where `read_output`
shows the empty composer's hint (`❯ Start a message with ! to run a shell command
yourself`); the fixture follows the recorded row.

## Extraction rule

Each fixture is exactly 16 lines, LF-terminated, with no trailing whitespace
beyond what the terminal itself painted. When a screen's content does not reach
16 rows, the extraction yields the top rows of that screen rather than a padded
window, so some fixtures start with blank lines or end with blank lines.

## Redactions

Text that would tie a fixture to one operator's machine is replaced with a fixed
dummy. The replacement keeps the line's character width, padding with spaces
right after the dummy, so the parts after it and the wrap of long paths stay in
the same columns.

- The folder in the footer (a temporary folder, shown abbreviated to the
  initials of each directory) is replaced with `~/work/project`.
- The same footer under WSL, abbreviated the same way and naming the user, is
  replaced with `/mnt/c/Users/demo/work/project`.
- The folder in the trust prompt (two wrapped rows) is replaced with
  `C:\Users\demo\work\project` on the first row and a blank second row, and its
  `Trust target:` path with `\\?\C:\Users\demo\work\project` the same way
  (`/mnt/c/Users/demo/work/project` in `100x30-wsl-trust`, which has no
  `Trust target:` row).

- In the `v1420` fixtures the folder in the footer
  (`~/github_project\yagura_test`) is replaced with `~/work/project` the same way.

Nothing else in the captured text was altered.
