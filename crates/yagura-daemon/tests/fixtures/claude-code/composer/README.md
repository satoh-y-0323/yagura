# Claude Code composer captures

Raw PTY byte streams (cumulative from process start) fed to a `VtScreen` by
`tests/composer_readout.rs`. Capture: Claude Code v2.1.285, Windows. The user
name and e-mail in title escapes were replaced by same-length dummies.

| fixture | cols x rows | screen |
|---|---|---|
| 100x30-wrapped-body-with-arrow.bin | 100x30 | 765-byte body wrapped over 9 rows; a wrapped row contains `->` |
| 100x30-partial-body-after-ctrl-u.bin | 100x30 | same body after Ctrl+U bursts; first row (63 bytes) left |
| 100x30-empty.bin | 100x30 | startup, empty composer with a dim hint |
| 80x24-marker-scrolled-off.bin | 80x24 | two long bodies; the marker row has scrolled above the screen |
| 100x30-waiting-body-do-you-want-to.bin | 100x30 | waiting screen; the composer body contains the dialog phrase `Do you want to` |
| 80x24-grown-body-one-envelope.bin | 80x24 | composer grown by one long body; no rule matches the bottom lines |
| 100x30-grown-body-two-envelopes.bin | 100x30 | composer grown by two long bodies |
| 100x30-running-body-flambeing.bin | 100x30 | running, spinner verb `Flambéing…` (non-ASCII), body in the composer |
| 100x30-running-body-flambeing-second-line.bin | 100x30 | running, non-ASCII spinner verb, longer body |
| 100x30-permission-dialog.bin | 100x30 | permission dialog (cursor hidden) |
| 80x24-permission-dialog.bin | 80x24 | permission dialog (cursor hidden) |
| 100x30-rewind-dialog.bin | 100x30 | Rewind dialog opened with Esc Esc (cursor hidden) |
| 100x30-finished-baked-for.bin | 100x30 | finished turn, completion line `Baked for <n>s` |
| 100x30-finished-worked-for.bin | 100x30 | finished turn, completion line `Worked for <n>s` |
