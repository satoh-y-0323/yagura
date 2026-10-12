# Antigravity CLI composer captures

Raw PTY byte streams (cumulative from process start) fed to a `VtScreen` by
`tests/composer_readout.rs`. Capture: Antigravity CLI 1.2.14, Windows. The
user name and e-mail in the banner were replaced by same-length dummies.

| fixture | cols x rows | screen |
|---|---|---|
| 100x30-wrapped-body-with-arrow.bin | 100x30 | 764-byte body wrapped over 10 rows; a wrapped row contains `->` |
| 100x30-empty.bin | 100x30 | startup, empty composer |
| 100x30-waiting-body-task-count.bin | 100x30 | waiting screen; the composer body contains `3 task(s)` |
| 100x30-grown-body-two-envelopes.bin | 100x30 | composer grown by two long bodies |
| 100x30-permission-dialog.bin | 100x30 | permission dialog (cursor hidden) |
| 100x30-permission-dialog-empty-composer.bin | 100x30 | permission dialog opened with an empty composer |
| 80x24-permission-dialog.bin | 80x24 | permission dialog (cursor hidden) |
| 100x30-folded-body-blank-lines.bin | 100x30 | waiting; a 40-line draft typed with Alt+Enter, every fifth line blank, folded under `> ↑ 25 more lines`; the cursor rests on a blank body line |
| 100x30-running-folded-body-blank-lines.bin | 100x30 | the same draft typed while a turn is generating (spinner above the composer) |
| 100x30-done-folded-body-blank-lines.bin | 100x30 | the same draft after that turn finished |
