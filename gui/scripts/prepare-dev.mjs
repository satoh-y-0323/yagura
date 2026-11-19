#!/usr/bin/env node
// The dev run's `beforeDevCommand`: builds the binaries the GUI starts from beside itself into the
// dev run's target directory and, on Windows, places the bundled ConPTY there too.
import { join } from 'node:path';
import {
  WORKSPACE_ROOT,
  buildBinPackages,
  cargoTargetDirectory,
  exitWith,
  isDevRunTargetDirectory,
  runOrExit,
} from './workspace-bins.mjs';

const TOOL = 'prepare-dev';

// A plain `tauri dev` builds into the default target, whose binaries every other build and test
// replaces; refuse it before building anything.
const targetDirectory = cargoTargetDirectory(TOOL);
if (!isDevRunTargetDirectory(targetDirectory)) {
  exitWith(
    TOOL,
    `the target directory ${targetDirectory} is not the dev run's; start the dev run with ` +
      '`npm run dev`, which runs `tauri dev` with its own target directory.'
  );
}

// Always debug: `tauri dev --release` reports a debug run to this hook, and `npm run dev` refuses it.
buildBinPackages(TOOL, { release: false });

// The pins and the placing live in yagura-test-support; this only hands it the directory.
if (process.platform === 'win32') {
  const besideGui = join(targetDirectory, 'debug');
  const result = runOrExit(
    TOOL,
    'cargo run',
    'cargo',
    ['run', '-p', 'yagura-test-support', '--bin', 'place-bundled-conpty', '--', besideGui],
    { cwd: WORKSPACE_ROOT }
  );
  if (result.status !== 0) {
    exitWith(
      TOOL,
      `placing the bundled ConPTY in ${besideGui} failed; see the output above for the reason.`,
      result.status ?? 1
    );
  }
}
