# Building from Source

This guide is for building yagura yourself: to run it on macOS or Linux, where no installer is
provided yet, or to work on yagura itself.

## Prerequisites

- Rust 1.88 or later (`rust-toolchain.toml` selects the stable channel)
- Node.js 24 or later, for the GUI
- The system libraries Tauri 2 needs on your OS -- see
  [Tauri's prerequisites](https://v2.tauri.app/start/prerequisites/). On Windows this is the
  WebView2 runtime (preinstalled on Windows 11); on Linux it includes the WebKitGTK development
  packages.

## Build Steps

```bash
# Stage the daemon and the MCP bridge as the GUI's sidecars (required before any cargo build)
cd gui
npm install
node scripts/prepare-sidecar.mjs

# Build and run the GUI in development mode; it starts the daemon for you
npx tauri dev

# Or build everything at once
cd ..
cargo build --workspace
```

**`cargo build` depends on the staged sidecars.** `gui/src-tauri/tauri.conf.json` declares
`bundle.externalBin = ["binaries/yagura-daemon", "binaries/yagura-mcp"]`, and `tauri-build`
resolves that at *cargo build* time: with no `gui/src-tauri/binaries/<name>-<host-triple>` (`.exe`
on Windows) present, the `yagura-gui` build script fails, and since `yagura-gui` is a workspace
member that takes `cargo build`/`clippy`/`test --workspace` down with it. `binaries/` is a build
product and is not tracked, so run `node scripts/prepare-sidecar.mjs` (from `gui/`) once after a
fresh clone. The script is self-contained and idempotent: it runs
`cargo build --release -p yagura-daemon -p yagura-mcp` itself and copies each executable under the
host-triple name Tauri expects.

**After changing the daemon or the MCP bridge, re-run it.** `tauri-build` copies the staged
sidecars next to the GUI executable on every cargo build, including `target/debug/`. Until
`prepare-sidecar.mjs` is re-run, a `tauri dev` run keeps launching the *previously staged release*
binaries, not your rebuilt ones.

**If a running MCP bridge or daemon holds the executable open, `cargo build` fails to link it.**
Use `scripts/stash-rebuild/stash_rebuild.py` to rename the held executable out of the way before
building; the process that holds it keeps running from the renamed file, and the rebuilt binary
takes effect the next time that process is started. See `scripts/stash-rebuild/README.md` for
details.

**On Windows, the daemon's build places the bundled ConPTY.** Every Windows window runs under the
ConPTY that ships with yagura rather than the one built into Windows. Its two files, `conpty.dll`
and `OpenConsole.exe` (`Microsoft.Windows.Console.ConPTY` 1.25.260930003, x64, MIT), are tracked
in `vendor/conpty/x64/`; `vendor/conpty/README.md` records where they come from and their SHA-256.
The `yagura-daemon` build script checks both files against the hashes pinned in
`crates/yagura-os/src/bundled_conpty_pins.rs` and fails the build when either is missing or
different. It then copies them into the Cargo profile directory (for example `target/debug/`),
beside the daemon's executable, and into its `deps/`, beside the test executables. A copy that
already has the pinned contents is left alone, so a running daemon that holds it open does not
break the build. The files are vendored for x86_64 only, so a Windows build for another
architecture fails. At run time the daemon checks the same pins beside its own executable, and
that the `conpty.dll` it loaded is that one; if not, it opens no window and reports
`spawn_failed` asking for a reinstall, stays running, and lists the problem in its
`get_daemon_health` answer, which the GUI shows as a banner. To update the ConPTY, follow `vendor/conpty/README.md` and
change the hashes in both places.

**Before pushing, check the dependencies for known vulnerabilities** with
`python scripts/dep-audit/dep_audit.py`, which runs `cargo audit` and `npm audit` with the same
criteria CI uses, and checks that the files in `vendor/conpty/` match the hashes recorded in its
README and pinned in the code. See `scripts/dep-audit/README.md` for the prerequisites, what fails,
and what to do when it does.

**To drive yagura from Claude Code while working on this repository, register the MCP bridge
yourself after a fresh clone.** The registration points at a build you produce locally, so it is
not tracked (`.mcp.json` is git-ignored):

```bash
cargo build -p yagura-mcp
claude mcp add yagura -s project -- ./target/debug/yagura-mcp.exe
```

That writes `.mcp.json` in the repository root and scopes the server to this project alone, leaving
your user-wide configuration untouched. Claude Code asks you to approve a project-scoped server the
first time it starts afterwards. On macOS and Linux drop the `.exe` suffix. To keep a development
build apart from the yagura you use every day, run it as a separate instance (see
[instances.md](instances.md)).

## Building the Installers

```bash
cd gui
npm ci
npm run build:web
npx tauri build
```

`tauri build` stages the sidecars itself (its `beforeBuildCommand` runs
`node scripts/prepare-sidecar.mjs`) and bundles for the OS it runs on. `tauri.conf.json` holds
the settings every OS shares; `tauri.windows.conf.json`, `tauri.macos.conf.json` and
`tauri.linux.conf.json` add each OS's targets and settings on top of it. The output lands under
`target/release/bundle/`:

| OS | Output | Notes |
|---|---|---|
| Windows | `nsis/yagura_<version>_x64-setup.exe` | See below. |
| macOS | `macos/yagura.app`, `dmg/yagura_<version>_aarch64.dmg` | Signed ad-hoc; see [macos-signing.md](macos-signing.md). Over SSH, set `CI=true`. |
| Linux | `deb/yagura_<version>_amd64.deb` | Needs the WebKitGTK 4.1 development packages (`libwebkit2gtk-4.1-dev` on Debian and Ubuntu). No AppImage: it runs the app from a different temporary path on every launch, and yagura recognizes its own GUI and MCP bridge by their paths. |

Pushing a `v*` tag runs the same build on each OS in CI (`.github/workflows/release.yml`) and
attaches the results to a draft release.

**The Windows installer** installs per user
(`nsis.installMode = "currentUser"`, no administrator rights), bundles `yagura-daemon.exe` and
`yagura-mcp.exe` alongside the GUI together with the bundled ConPTY's `conpty.dll` and
`OpenConsole.exe` (`bundle.resources` in `tauri.windows.conf.json`), and installs the WebView2 runtime silently when the machine
lacks it (that step needs network access). The detection definitions are compiled into
`yagura-daemon.exe` itself, so the installer puts none of them on disk; definitions of your own go
in a `detection` directory under the config root (`%LOCALAPPDATA%\io.github.satoh-y-0323.yagura` on Windows), where they
override the compiled-in ones name by name. The build is unsigned, so SmartScreen warns about an
unrecognized publisher on first run. Stop any running `yagura-daemon.exe` first: a live daemon
holds a lock on its executable in the target directory and fails the build.

## Regenerating the Application Icon

The icon scripts use GDI+ and run on Windows only.

`gui/src-tauri/icons/icon.svg` is the icon source (a restricted SVG subset -- see the
comment-based help in `render-icon.ps1` for the exact element/attribute list: `linearGradient`/
`stop`, `path` with `M`/`L`/`l` and round caps/joins, `rect`, `circle`). `icons/icon-small.svg` is
a simplified glyph, used only for the `.ico`'s 16x16/24x24/32x32 entries so the icon stays legible
at small sizes -- multi-size `.ico` files routinely carry a distinct image per size. Regenerating
the full icon set after editing either SVG is a three-step pipeline, each step run from the
repository root:

```bash
powershell -ExecutionPolicy Bypass -File gui/scripts/render-icon.ps1
cd gui/src-tauri && npx @tauri-apps/cli icon app-icon.png -o icons && cd ../..
powershell -ExecutionPolicy Bypass -File gui/scripts/compose-ico.ps1
```

1. `render-icon.ps1` (no CWD requirement) rasterizes `icon.svg` to the 1024x1024 `app-icon.png`
   without anti-aliasing. GDI+ has no SVG rasterizer, so this project holds its own restricted
   subset rather than committing a rendered PNG as the source of truth.
2. `tauri icon` (CWD = `gui/src-tauri`) turns `app-icon.png` into the full platform icon set,
   including `icons/icon.ico`. It also generates Android and iOS icon sets; the desktop bundles
   do not reference them, but they stay tracked so a regeneration produces no diff.
3. `compose-ico.ps1` (no CWD requirement) must run *after* step 2: it splices simplified-glyph
   renders of `icon-small.svg` into `icon.ico`'s 16x16/24x24/32x32 entries, which `tauri icon`
   itself always derives from the master SVG instead. `gui/scripts/ico-common.ps1` holds the
   helpers shared between all three scripts (multi-stage downscale, PNG encoding, ICO
   parsing/assembly, and output-path resolution/validation); it defines functions only and is
   never run directly.

All three scripts run as `powershell -ExecutionPolicy Bypass -File ...` rather than `pwsh`:
`Add-Type -AssemblyName System.Drawing` (GDI+) does not reliably resolve under PowerShell 7's
.NET runtime, only under Windows PowerShell 5.1. Internally, the child-process calls the scripts
make of each other launch via the absolute path of Windows PowerShell 5.1 rather than relying on
PATH resolution.

This pipeline is deterministic end to end: re-running all three steps reproduces every generated
file byte-for-byte (compare against a scratch copy, not `git diff` -- the tracked files are
already the reproduced state, so a diff shows nothing either way), **except `icons/icon.icns`**:
two consecutive `tauri icon` runs on the same input produce an `icon.icns` of the same length with
different bytes. `.icns` has no pixel-level comparator available here, so `icon.icns` is excluded
from `verify-icon.ps1`'s measurements and from the byte-reproducibility claim above.

`gui/scripts/verify-icon.ps1 -ScratchDir <scratch>` checks this end to end with six measurements
(master render determinism, full-pipeline ICO/PNG integrity, ICO structure, transparency, the
simplified-glyph entries, and a master pixel spot-check) and exits non-zero if any of them fails.
`-AppIconPath` / `-IcoPath` default to the tracked assets and are read-only; `-ScratchDir` must
resolve outside the repository and must not resolve into it through a reparse point (junction or
symlink) either. The spot-check compares the master against expected pixel values frozen from the
current artwork, so a deliberate design change requires updating those expectations in
`verify-icon.ps1` along with the assets.

The same pipeline works entirely from scratch copies, without touching the tracked assets:

```bash
powershell -ExecutionPolicy Bypass -File gui/scripts/render-icon.ps1 -OutPath <scratch>\app-icon.png
cd gui && npx @tauri-apps/cli icon <scratch>\app-icon.png -o <scratch>\icons && cd ..
powershell -ExecutionPolicy Bypass -File gui/scripts/compose-ico.ps1 -SourceIco <scratch>\icons\icon.ico -OutPath <scratch>\icon.ico
```

(`-SourceIco` in the third command is the `icon.ico` the second command's `-o` just wrote.)

This assumes `npm install` has already been run in `gui/`. If you change icon assets (SVGs,
scripts), run `verify-icon.ps1` before committing.
