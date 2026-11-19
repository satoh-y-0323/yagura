#!/usr/bin/env node
// Stages the release binaries the distribution bundles as sidecars (`bundle.externalBin` of each
// OS's distribution config): the daemon, the holder the daemon starts for each window from beside
// itself, on Linux and macOS the watcher each holder starts from beside itself, and the MCP bridge.
// Only the distribution's bundle step reads the staged copies.
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import {
  WORKSPACE_ROOT,
  buildBinPackages,
  cargoTargetDirectory,
  exitWith,
} from './workspace-bins.mjs';

const TOOL = 'prepare-sidecar';
const binariesDir = join(WORKSPACE_ROOT, 'gui', 'src-tauri', 'binaries');

// The binaries staged as sidecars, in the order each platform's `bundle.externalBin`
// names them. The holder's package builds the watcher too; Windows ships none, its job
// object holding a window's tree.
const SIDECAR_BINARIES =
  process.platform === 'win32'
    ? ['yagura-daemon', 'yagura-holder', 'yagura-mcp']
    : ['yagura-daemon', 'yagura-holder', 'yagura-watcher', 'yagura-mcp'];

function fail(message) {
  exitWith(TOOL, message);
}

// Step 1: build every sidecar package, so a re-run always stages the current sources.
buildBinPackages(TOOL, { release: true });

// Step 2: locate the cargo target directory, shared by every staged package.
const targetDirectory = cargoTargetDirectory(TOOL);

// Step 3: the host triple Tauri expects each sidecar to be named with.
const hostLine = execFileSync('rustc', ['-vV'], { cwd: WORKSPACE_ROOT, encoding: 'utf8' })
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
for (const name of SIDECAR_BINARIES) {
  const source = join(targetDirectory, 'release', `${name}${exeSuffix}`);
  if (!existsSync(source)) {
    fail(`no ${name} binary at ${source} after a successful build; nothing to stage.`);
  }
  const destination = join(binariesDir, `${name}-${triple}${exeSuffix}`);
  copyFileSync(source, destination);
  console.log(`prepare-sidecar: staged ${source} -> ${destination}`);
}
