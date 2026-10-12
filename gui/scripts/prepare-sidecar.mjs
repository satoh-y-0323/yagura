#!/usr/bin/env node
// Stages the release binaries Tauri bundles as sidecars (`bundle.externalBin`):
// the daemon and the MCP bridge. `tauri-build` resolves `externalBin` at cargo
// build time, so every cargo build of the workspace depends on this having run.
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const workspaceRoot = resolve(scriptDir, '..', '..');
const binariesDir = join(workspaceRoot, 'gui', 'src-tauri', 'binaries');

// The packages staged as sidecars, in the order `bundle.externalBin` names them.
const SIDECAR_PACKAGES = ['yagura-daemon', 'yagura-mcp'];

function fail(message) {
  console.error(`prepare-sidecar: ${message}`);
  process.exit(1);
}

function capture(command, args) {
  return execFileSync(command, args, {
    cwd: workspaceRoot,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
}

// Step 1: build every sidecar package, so a re-run always stages the current sources.
try {
  execFileSync(
    'cargo',
    ['build', '--release', ...SIDECAR_PACKAGES.flatMap((name) => ['-p', name])],
    { cwd: workspaceRoot, stdio: 'inherit' }
  );
} catch {
  fail(
    'cargo build --release failed; see the cargo output above for the reason. ' +
      'It may be caused by a running yagura-daemon holding a lock on its executable in the ' +
      'target directory (os error 5); if so, stop it and re-run.'
  );
}

// Step 2: locate the cargo target directory, shared by every staged package.
let targetDirectory;
try {
  // --no-deps keeps the JSON small; target_directory is reported either way.
  const metadata = JSON.parse(capture('cargo', ['metadata', '--format-version', '1', '--no-deps']));
  targetDirectory = metadata.target_directory;
} catch {
  fail('cargo metadata --format-version 1 failed; cannot locate the cargo target directory.');
}

// Step 3: the host triple Tauri expects each sidecar to be named with.
const hostLine = capture('rustc', ['-vV'])
  .split('\n')
  .map((line) => line.trim())
  .find((line) => line.startsWith('host: '));
if (!hostLine) {
  fail('rustc -vV reported no host triple; cannot name the sidecars.');
}
const triple = hostLine.slice('host: '.length).trim();

// Windows binaries carry the `.exe` extension; Unix (Linux/macOS) ones do not.
const exeSuffix = process.platform === 'win32' ? '.exe' : '';

// Step 4: copy each built binary under the host-triple name Tauri expects.
mkdirSync(binariesDir, { recursive: true });
for (const name of SIDECAR_PACKAGES) {
  const source = join(targetDirectory, 'release', `${name}${exeSuffix}`);
  if (!existsSync(source)) {
    fail(`no ${name} binary at ${source} after a successful build; nothing to stage.`);
  }
  const destination = join(binariesDir, `${name}-${triple}${exeSuffix}`);
  copyFileSync(source, destination);
  console.log(`prepare-sidecar: staged ${source} -> ${destination}`);
}
