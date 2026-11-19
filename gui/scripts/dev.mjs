#!/usr/bin/env node
// The dev entry point (`npm run dev`): runs `tauri dev` with the dev run's own target directory, so
// the GUI and the binaries prepare-dev.mjs builds beside it are written by this entry point alone.
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import {
  WORKSPACE_ROOT,
  cargoTargetDirectory,
  devRunTargetDirectory,
  exitWith,
} from './workspace-bins.mjs';

const TOOL = 'dev';
const guiDir = join(WORKSPACE_ROOT, 'gui');
const tauriCli = join(guiDir, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');

// `tauri dev --release` still tells beforeDevCommand it is a debug run, so the preparation could not
// build the profile the GUI runs from; the dev run is debug only.
if (process.argv.length > 2) {
  exitWith(
    TOOL,
    `takes no arguments (got: ${process.argv.slice(2).join(' ')}); the dev run is a debug ` +
      'build of the target directory cargo resolves (set CARGO_TARGET_DIR to pick another).'
  );
}

// The caller's target directory decides where the dev run lives, so separate targets keep
// separate dev runs.
const devRunTarget = devRunTargetDirectory(cargoTargetDirectory(TOOL));

const result = spawnSync(process.execPath, [tauriCli, 'dev'], {
  cwd: guiDir,
  stdio: 'inherit',
  env: { ...process.env, CARGO_TARGET_DIR: devRunTarget },
});
if (result.error) {
  exitWith(TOOL, `tauri dev could not start: ${result.error.message}`);
}
process.exit(result.status ?? 1);
