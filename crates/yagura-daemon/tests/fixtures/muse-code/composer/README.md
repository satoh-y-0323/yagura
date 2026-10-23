# Muse Code composer captures

Raw PTY byte streams (cumulative from process start) to be fed to a fresh
`VtScreen(100, 30)` for the composer readout. Capture: Muse Code 1.4.12
(`1.4.12-R6909.1`), Windows, on 2026-10-23, under the conditions in
`../README.md` (`PtySession::spawn_with_command_in_env` with
`DaemonConfig::default()`, color queries unanswered, `--trust-workspace`).

After a turn, an empty composer can show a hint where the draft goes, drawn in
a dim color only (no dim or italic attribute); the `placeholder-` captures hold
the three hints seen so far.

## Source

| fixture | cols x rows | expected | screen |
|---|---|---|---|
| 100x30-empty.bin | 100x30 | Empty | `--provider echo` through `muse.cmd`, the empty composer after start |
| 100x30-draft.bin | 100x30 | Body(`hello draft`) | the same window, `hello draft` typed and not sent |
| 100x30-multiline-draft.bin | 100x30 | Body(`fast-one`, `line-a`, `line-b`) | `--provider echo` started with the prompt `launch-hello-42`; after that turn, a draft of three lines joined with Ctrl+J |
| 100x30-placeholder-shell.bin | 100x30 | Empty | a model window after one Esc interrupted a running shell command; the composer shows `Start a message with ! to run a shell command yourself` |
| 100x30-placeholder-mention.bin | 100x30 | Empty | `--provider echo` after one turn; the composer shows `Type @ to search and insert workspace file paths` |
| 100x30-placeholder-monitor.bin | 100x30 | Empty | a model window started directly with per-run `-c` values, after a turn whose MCP tool call timed out; the composer shows `Ask to monitor a running build or log and get pinged on changes` |

## Redactions

The folder in the footer (a temporary folder, shown abbreviated to the initials
of each directory) is replaced with `~/work/project`, padded with spaces to the
same number of ASCII bytes, wherever it was drawn. In `100x30-placeholder-monitor.bin` one redraw
wrote that path in two pieces around a cursor move; each piece takes the
dummy's characters at the same offsets, so the skipped cell is a space in both
the old and the new drawing. No other byte was changed; the window titles
(OSC 0) keep the folder's base name.
