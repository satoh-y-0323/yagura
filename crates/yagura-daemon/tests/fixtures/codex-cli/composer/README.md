# Codex composer captures

Raw PTY byte streams (cumulative from process start) fed to a `VtScreen` by
`tests/detector_composer_masking.rs`. Capture: Codex CLI v0.159.2, Windows.

| fixture | cols x rows | screen |
|---|---|---|
| 100x30-waiting-body-press-enter.bin | 100x30 | waiting screen; the composer body contains `Press enter to continue` |
| 100x30-grown-body-two-envelopes.bin | 100x30 | composer grown by two long bodies |
| 100x30-permission-dialog.bin | 100x30 | permission dialog (cursor hidden) |
| 100x30-permission-dialog-empty-composer.bin | 100x30 | permission dialog opened with an empty composer |
| 80x24-permission-dialog.bin | 80x24 | permission dialog (cursor hidden) |
