# Codex composer captures

Raw PTY byte streams (cumulative from process start) fed to a `VtScreen` by
`tests/composer_readout.rs`. Capture: Codex CLI v0.159.2, Windows.

| fixture | cols x rows | screen |
|---|---|---|
| 100x30-wrapped-body-with-arrow.bin | 100x30 | 767-byte body wrapped over 9 rows; a wrapped row contains `->` |
| 100x30-empty.bin | 100x30 | startup, empty composer |
| 100x30-waiting-body-press-enter.bin | 100x30 | waiting screen; the composer body contains `Press enter to continue` |
| 100x30-grown-body-two-envelopes.bin | 100x30 | composer grown by two long bodies |
| 100x30-permission-dialog.bin | 100x30 | permission dialog (cursor hidden) |
| 100x30-permission-dialog-empty-composer.bin | 100x30 | permission dialog opened with an empty composer |
| 80x24-permission-dialog.bin | 80x24 | permission dialog (cursor hidden) |
