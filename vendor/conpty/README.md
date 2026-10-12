# Bundled ConPTY

The pseudoconsole the daemon runs every Windows window under, instead of the
one built into Windows. The built-in ConPTY answers a child's terminal
queries itself and repaints its own screen after a resize; this one leaves
both to the terminal on the other side, which is the daemon's model.

- Source: NuGet package `Microsoft.Windows.Console.ConPTY` 1.25.260930003
  (microsoft/terminal, MIT, see `LICENSE`)
- `x64/conpty.dll`: `runtimes/win-x64/native/conpty.dll`
- `x64/OpenConsole.exe`: `build/native/runtimes/x64/OpenConsole.exe`

| File | SHA-256 |
|---|---|
| `x64/conpty.dll` | `feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a` |
| `x64/OpenConsole.exe` | `3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586` |

The package's SHA-512 matched the `packageHash` NuGet publishes for this
version, and both files carry a valid Authenticode signature from Microsoft
Corporation. To update, take the files from a new package version, check the
same two things, and replace the hashes here and in
`crates/yagura-os/src/bundled_conpty_pins.rs`, which the build script, the
runtime check and dep-audit all read.
