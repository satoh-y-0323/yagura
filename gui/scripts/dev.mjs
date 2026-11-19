#!/usr/bin/env node
// The dev entry point (`npm run dev`): runs `tauri dev` with the dev run's own target directory, so
// the GUI and the binaries prepare-dev.mjs builds beside it are written by this entry point alone.
import {
  cargoTargetDirectory,
  devRunTargetDirectory,
  refuseArguments,
  runTauri,
} from './workspace-bins.mjs';

const TOOL = 'dev';

// `tauri dev --release` still tells beforeDevCommand it is a debug run, so the preparation could not
// build the profile the GUI runs from; the dev run is debug only.
refuseArguments(
  TOOL,
  'the dev run is a debug build of the target directory cargo resolves (set CARGO_TARGET_DIR to ' +
    'pick another).'
);

// The caller's target directory decides where the dev run lives, so separate targets keep
// separate dev runs.
const devRunTarget = devRunTargetDirectory(cargoTargetDirectory(TOOL));

runTauri(TOOL, 'tauri dev', ['dev'], {
  env: { ...process.env, CARGO_TARGET_DIR: devRunTarget },
  reportFailure: false,
});
