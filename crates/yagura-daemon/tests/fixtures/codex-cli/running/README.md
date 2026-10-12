# Codex running-state captures

Raw PTY byte streams (cumulative from process start) fed to a `DetectionEngine`
by `tests/detector_codex_running.rs`. Capture: Codex CLI v0.159.2, Windows,
model GPT-5.6-Luna. Each file is the stream up to one moment of a scripted
session; the moment is the file name.

| fixture | cols x rows | screen |
|---|---|---|
| 100x30-working-empty-composer.bin | 100x30 | `Working (... esc to interrupt)` line, empty composer, 3 s into a turn |
| 80x24-working-empty-composer.bin | 80x24 | same at 80x24 |
| 100x30-working-with-body.bin | 100x30 | `Working (...)` line and a body typed into the composer while the turn runs |
| 100x30-body-no-working-line.bin | 100x30 | answer streaming, body in the composer, no `Working` line, `tab to queue message` shown |
| 80x24-body-no-working-line.bin | 80x24 | same at 80x24 |
| 50x20-body-no-working-line.bin | 50x20 | same at 50x20 |
| 100x30-idle-startup.bin | 100x30 | startup, empty composer |
| 100x30-idle-after-answer.bin | 100x30 | waiting for input after an answer (`Worked for ...`) |
| 100x30-idle-typed.bin | 100x30 | waiting for input, body typed into the composer |
| 80x24-idle-typed.bin | 80x24 | same at 80x24 |
| 100x30-idle-background-terminal.bin | 100x30 | waiting for input while `1 background terminal running` |
| 100x30-idle-background-terminal-typed-hint.bin | 100x30 | same, with the words `tab to queue message` typed into the composer (cursor visible) |
| 100x30-approval-dialog.bin | 100x30 | file-write approval dialog (cursor hidden) |
| 80x24-approval-dialog.bin | 80x24 | same at 80x24 |
| 100x30-approval-dialog-with-body.bin | 100x30 | approval dialog opened while the composer held a body |
