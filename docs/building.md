# Building from Source

This guide is for building yagura yourself: to run it on macOS or Linux, where no installer is
provided yet, or to work on yagura itself.

## Prerequisites

- Rust 1.89 or later (`rust-toolchain.toml` selects the stable channel)
- Node.js 24 or later, for the GUI
- The system libraries Tauri 2 needs on your OS -- see
  [Tauri's prerequisites](https://v2.tauri.app/start/prerequisites/). On Windows this is the
  WebView2 runtime (preinstalled on Windows 11); on Linux it includes the WebKitGTK development
  packages.

## Build Steps

```bash
cd gui
npm install
npm run build:web   # the GUI crate embeds gui/dist, so build it before any cargo build

# Run this checkout's GUI, daemon and MCP bridge as the `dev` instance
YAGURA_INSTANCE=dev npm run dev   # PowerShell: $env:YAGURA_INSTANCE = 'dev'; npm run dev

# Or build everything at once
cd ..
cargo build --workspace
```

**Nothing has to be staged first.** Once `gui/dist` exists, a fresh clone builds with
`cargo build`, `clippy` and `test --workspace`. What a distribution ships beside the GUI -- the
daemon, the holder, on Linux and macOS the watcher, the MCP bridge, and on Windows the bundled
ConPTY -- is named only in the distribution configs that `npm run build:tauri` hands to the bundle
step (see [Building the Installers](#building-the-installers)); no cargo build reads them. Run
`npm run build:web` again after changing the frontend.

**Do not put distribution settings in a file tauri-build reads.** On every cargo build of the GUI,
tauri-build reads `gui/src-tauri/tauri.conf.json` and, when one exists,
`tauri.<platform>.conf.json` (`tauri.windows.conf.json` and so on), and copies each
`bundle.externalBin` and `bundle.resources` entry into the cargo target directory. There the copies
contend with the executables cargo itself builds for every other build and test, and an entry that
does not exist yet fails every cargo build of the workspace. Keep `externalBin` and `resources` in
the `tauri.bundle.<os>.conf.json` files, and do not create a `tauri.<platform>.conf.json`.

### Running a development build

`npm run dev` (from `gui/`) runs `tauri dev` in a cargo target directory of its own: `dev-run`
under the one cargo resolves, so `target/dev-run/` unless `CARGO_TARGET_DIR` or
`build.target-dir` in `.cargo/config.toml` points elsewhere. Before it builds the GUI, its
`beforeDevCommand` (`gui/scripts/prepare-dev.mjs`) builds the daemon, the holder (with the watcher
on Linux and macOS) and the MCP bridge into the same directory and, on Windows, places the bundled
ConPTY beside them. A daemon the GUI starts is therefore the one beside it in
`target/dev-run/debug/`, built from this checkout. The dev run is a debug build, and `npm run dev`
takes no arguments.

- Only `npm run dev` writes to `dev-run/`. Builds and tests of the default target never touch it,
  so you can run `cargo build` and `cargo test --workspace` while a dev run and its windows keep
  running.
- A plain `npx tauri dev` stops before building anything: the preparation refuses a target
  directory that is not the dev run's and names `npm run dev`.
- To start the GUI that `npm run dev` built without `tauri dev`, run
  `target/dev-run/debug/yagura-gui` (`yagura-gui.exe` on Windows). To run the preparation by hand,
  run `node gui/scripts/prepare-dev.mjs` with `CARGO_TARGET_DIR` set to that `dev-run` directory.

**The daemon outlives the GUI** (see [mcp-setup.md](mcp-setup.md#starting-the-daemon)), and a dev
run follows the same rule: the GUI starts a daemon only when none is running for its instance, and
otherwise connects to the one that is, whatever build it is. A daemon that answers another IPC wire
revision than the GUI's is not used: the GUI shows no windows and asks you to restart it. Three
things follow:

1. **Run the dev build as an instance of its own** (`YAGURA_INSTANCE=dev`). In the default
   instance, a dev run connects to the daemon of the yagura you use every day when it is running,
   and starts no daemon of its own. A daemon accepts as its approver only the GUI it was started
   for (`--approver-exe`), so the dev GUI cannot answer that daemon's approvals either; a daemon
   started without `--approver-exe`, for example from a command line, refuses every GUI.
2. **After changing the daemon, the holder or the MCP bridge** -- or a crate they share with the
   GUI, such as `yagura-os` or `yagura-protocol` -- restart the dev run in this order:
   1. Close that instance's GUI; for the dev run, stop `npm run dev`. An open GUI starts the daemon
      again as soon as it stops.
   2. Stop the daemon: `YAGURA_INSTANCE=dev target/dev-run/debug/yagura-daemon --shutdown`. It
      returns once the daemon process has ended, and the daemon closes every window it runs.
   3. Run `npm run dev` again.

   `tauri dev` does not rebuild the daemon, the holder or the bridge while it runs, and runs its
   `beforeDevCommand` only when it starts. If the daemon is still running when the preparation
   rebuilds it, on Windows the preparation stops: the running daemon holds its executable, and the
   link fails with the steps above. On macOS and Linux the build replaces the file, and the running
   daemon carries on with the old code.
3. **After changing only the GUI** (`gui/`), the daemon may keep running. `tauri dev` rebuilds and
   restarts the GUI on its own when `gui/src-tauri` changes, or restart `npm run dev`; the new GUI
   connects to the running daemon and its windows.

`scripts/stash-rebuild/stash_rebuild.py` is not needed for a dev run. It renames an executable a
running process holds out of the way so that `cargo build` can link a fresh one into the default
target -- for example a daemon you keep running straight from `target/<profile>/` -- and the
rebuilt binary takes effect the next time that process starts. See
`scripts/stash-rebuild/README.md`.

**On Windows, every window runs under the ConPTY that ships with yagura.** It is used instead of
the one built into Windows. Its two files, `conpty.dll` and `OpenConsole.exe`
(`Microsoft.Windows.Console.ConPTY` 1.25.260930003, x64, MIT), are tracked in `vendor/conpty/x64/`;
`vendor/conpty/README.md` records where they come from and their SHA-256. The `yagura-daemon` and
`yagura-holder` build scripts check both files against the hashes pinned in
`crates/yagura-os/src/bundled_conpty_pins.rs` and fail the build when either is missing or
different; they copy nothing, because a build script has no supported way to find the target
directory (cargo#9661, which took over cargo#5457, closed as not planned). The files are vendored
for x86_64 only, so a Windows build for another architecture fails.

The files reach a daemon's directory in one of these ways:

- A dev run's preparation places them in `dev-run/debug/` with the same check and placing the tests
  use (the `place-bundled-conpty` command of `yagura-test-support`).
- The tests do not rely on any copy. Before starting a holder they check the pair beside it
  against the pins and, when it does not match, place it from `vendor/conpty/`.
- To run a daemon straight from `target/<profile>` on Windows, copy `vendor/conpty/x64/conpty.dll`
  and `vendor/conpty/x64/OpenConsole.exe` next to `yagura-daemon.exe` (and so `yagura-holder.exe`).
  Without them the daemon opens no window and says so in `get_daemon_health`.
- The installer carries them through `bundle.resources` in `tauri.bundle.windows.conf.json`.

At run time the daemon checks the same pins beside its own executable, and that the `conpty.dll` it
loaded is that one; if not, it opens no window and reports `spawn_failed` asking for a reinstall,
stays running, and lists the problem in its `get_daemon_health` answer, which the GUI shows in the
strip at the top. To update the ConPTY, follow `vendor/conpty/README.md` and change the hashes in both places.

**Before pushing, check the dependencies for known vulnerabilities** with
`python scripts/dep-audit/dep_audit.py`, which runs `cargo audit` and `npm audit` with the same
criteria CI uses, and checks that the files in `vendor/conpty/` match the hashes recorded in its
README and pinned in the code. See `scripts/dep-audit/README.md` for the prerequisites, what fails,
and what to do when it does.

**To drive yagura from Claude Code while working on this repository, register the dev run's MCP
bridge yourself after a fresh clone.** The registration points at a build you produce locally, so
it is not tracked (`.mcp.json` is git-ignored). Run `npm run dev` once to build the bridge, then,
from the repository root:

```bash
claude mcp add --scope project --env YAGURA_INSTANCE=dev yagura-dev -- ./target/dev-run/debug/yagura-mcp.exe
```

That writes `.mcp.json` in the repository root and scopes the server to this project alone, leaving
your user-wide configuration untouched. Claude Code asks you to approve a project-scoped server the
first time it starts afterwards. On macOS and Linux drop the `.exe` suffix. The name and the
`YAGURA_INSTANCE` follow the dev instance, as [Registering from the GUI](mcp-setup.md#registering-from-the-gui)
does (see [instances.md](instances.md)). After changing the bridge's source, close the Claude Code
session that runs it before running `npm run dev`: on Windows the session holds the executable, and
the preparation cannot replace it.

## Building the Installers

```bash
cd gui
npm ci
npm run build:web
npm run build:tauri
```

`npm run build:tauri` (`gui/scripts/build-bundle.mjs`) bundles in two steps. `tauri build
--no-bundle` builds the GUI in release, and its `beforeBuildCommand` runs
`node scripts/prepare-sidecar.mjs`, which builds the release daemon, holder, watcher and bridge and
stages them in `gui/src-tauri/binaries/` under the host-triple names Tauri expects. Then
`tauri bundle --config` bundles with this OS's distribution config. When either step fails, the
command names it and exits non-zero. It takes no arguments: it bundles for the host triple, the
only one the sidecars are staged for. The output lands under `target/release/bundle/`:

| OS | Output | Notes |
|---|---|---|
| Windows | `nsis/yagura_<version>_x64-setup.exe` | See below. |
| macOS | `macos/yagura.app`, `dmg/yagura_<version>_aarch64.dmg` | Signed ad-hoc; see [macos-signing.md](macos-signing.md). Over SSH, set `CI=true`. Needs Xcode 26 or later for the macOS 26 icon (see below). |
| Linux | `deb/yagura_<version>_amd64.deb` | Needs the WebKitGTK 4.1 development packages (`libwebkit2gtk-4.1-dev` on Debian and Ubuntu). No AppImage: it runs the app from a different temporary path on every launch, and yagura recognizes its own GUI and MCP bridge by their paths. |

`tauri.conf.json` holds the settings every OS shares and every cargo build reads. Each OS's
distribution config -- `tauri.bundle.windows.conf.json`, `tauri.bundle.macos.conf.json` or
`tauri.bundle.linux.conf.json` -- holds everything that OS's bundle adds on top of it: the bundle
targets, `externalBin`, and on Windows `resources` and the installer settings, on macOS the icon
and the signing identity. Only the bundle step reads them.

**A plain `npx tauri build` or `npx tauri bundle` makes no distribution.** The
`beforeBundleCommand` in `tauri.conf.json` (`gui/scripts/refuse-bundle.mjs`) stops it before
anything is bundled, naming `npm run build:tauri`, and each distribution config replaces that hook
(with none, or on macOS with the icon step). Keep the hook in `tauri.conf.json`: without it, a
bundle from `tauri.conf.json` alone succeeds and ships an app with none of its binaries beside it.

Pushing a `v*` tag runs the same build on each OS in CI (`.github/workflows/release.yml`) and
attaches the results to a draft release.

**The Windows installer** installs per user
(`nsis.installMode = "currentUser"`, no administrator rights), bundles `yagura-daemon.exe`,
`yagura-holder.exe` and `yagura-mcp.exe` alongside the GUI together with the bundled ConPTY's
`conpty.dll` and `OpenConsole.exe` (`bundle.resources` in `tauri.bundle.windows.conf.json`), and
installs the WebView2 runtime silently when the machine lacks it (that step needs network access).
The detection definitions are compiled into
`yagura-daemon.exe` itself, so the installer puts none of them on disk; definitions of your own go
in a `detection` directory under the config root (`%LOCALAPPDATA%\io.github.satoh-y-0323.yagura` on Windows), where they
override the compiled-in ones name by name. The build is unsigned, so SmartScreen warns about an
unrecognized publisher on first run. Stop any `yagura-daemon.exe` running from `target/release/`
first: a live daemon holds a lock on its executable there and fails the build.

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

### The macOS 26 icon

macOS 26 masks every app icon to its rounded square, and shows an icon of any other shape shrunk
onto a gray plate. `icons/yagura.icon` is an Icon Composer icon for macOS 26 and later: a dark blue
gradient fill with `icon.svg` as its only layer, scaled and offset to the centre in `icon.json`.
On macOS, the bundle step of `npm run build:tauri` runs `gui/scripts/compile-macos-icon.mjs` before
bundling (`beforeBundleCommand` in `tauri.bundle.macos.conf.json`). It compiles the icon with `actool`, which needs
Xcode 26 or later, into `gui/src-tauri/gen/macos-icon/Assets.car`, and the bundle lists that file.
Earlier macOS releases keep using `icons/icon.icns`. Tauri could compile the `.icon` itself, but under
the Node.js CLI it starts `actool` without a stdin and `actool` crashes
([tauri-apps/tauri#15315](https://github.com/tauri-apps/tauri/issues/15315)).

`icons/yagura.icon/Assets/icon.svg` is a copy of `icons/icon.svg`: copy it again after editing the
artwork.
