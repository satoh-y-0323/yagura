# Codex repaint capture

A raw PTY byte stream and the arrival time of every chunk of it, replayed by
`tests/detector_settled_screen.rs` against the bundled Codex definition.
Capture: Codex CLI v0.159.2, Windows, model GPT-5.6-Luna, 80x24, working
directory `C:\demo\todo-app`. The stream is cumulative from process start and
is cut at 15.5 s.

| file | content |
|---|---|
| 80x24-body-typed-during-turn.bin | the bytes, exactly as read from the PTY |
| 80x24-body-typed-during-turn.tsv | one line per chunk: arrival time in ms since the process started, tab, chunk length in bytes |

Scripted session: a question is submitted at about 4.5 s and the turn runs; a
body containing the words `tab to queue message` is typed into the composer
at about 10.2 s while the turn is still running. From about 10.5 s until the
body is cleared at about 14.8 s the screen shows `tab to queue message` and
is repainted many times a second; each repaint is wrapped in cursor hide/show
and some reads end inside it, where the half-drawn screen reads as an approval
dialog.

The capture contains no user name, address or credential.
