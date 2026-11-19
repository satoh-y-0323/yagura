// What the distribution staging (prepare-sidecar.mjs), the entry points (dev.mjs, build-bundle.mjs)
// and the dev preparation (prepare-dev.mjs) share: the packages built beside the GUI, the cargo
// target directory, the dev run's own target directory and how a script runs its tools.
import { execFileSync, spawnSync } from 'node:child_process';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const WORKSPACE_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
export const GUI_DIR = join(WORKSPACE_ROOT, 'gui');
const TAURI_CLI = join(GUI_DIR, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');

/** The packages whose binaries run beside the GUI; the holder's package builds the watcher too. */
const BIN_PACKAGES = ['yagura-daemon', 'yagura-holder', 'yagura-mcp'];

// The dev run builds only here, so the default target's builds and tests never replace the
// binaries a dev run's daemon is running from. Cargo puts no directory of this name in a target.
const DEV_RUN_DIR_NAME = 'dev-run';

export function exitWith(tool, message, code = 1) {
  console.error(`${tool}: ${message}`);
  process.exit(code);
}

/** Exits non-zero, naming what was given and `reason`, if the script was given any arguments. */
export function refuseArguments(tool, reason) {
  if (process.argv.length > 2) {
    exitWith(tool, `takes no arguments (got: ${process.argv.slice(2).join(' ')}); ${reason}`);
  }
}

/**
 * Runs `command` without a shell, inheriting stdio, and returns the finished result; exits
 * non-zero naming `stage` if it could not start.
 */
export function runOrExit(tool, stage, command, args, { cwd, env = process.env }) {
  const result = spawnSync(command, args, { cwd, env, stdio: 'inherit' });
  if (result.error) {
    exitWith(tool, `${stage} could not start: ${result.error.message}`);
  }
  return result;
}

/**
 * Runs the GUI's tauri CLI with `args` from the GUI directory and returns if it exits 0. Otherwise
 * exits with its code (1 for a signal), first naming `stage` and how it ended if `reportFailure`.
 */
export function runTauri(tool, stage, args, { env = process.env, reportFailure }) {
  const result = runOrExit(tool, stage, process.execPath, [TAURI_CLI, ...args], {
    cwd: GUI_DIR,
    env,
  });
  if (result.status === 0) {
    return;
  }
  const code = result.status ?? 1;
  if (reportFailure) {
    const how = result.status === null ? `signal ${result.signal}` : `exit code ${result.status}`;
    exitWith(tool, `${stage} failed with ${how}.`, code);
  }
  process.exit(code);
}

/** `target_directory` from `cargo metadata`, honouring CARGO_TARGET_DIR and `.cargo/config.toml`. */
export function cargoTargetDirectory(tool) {
  try {
    const metadata = execFileSync('cargo', ['metadata', '--format-version', '1', '--no-deps'], {
      cwd: WORKSPACE_ROOT,
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
    });
    return JSON.parse(metadata).target_directory;
  } catch {
    exitWith(
      tool,
      'cargo metadata --format-version 1 failed; cannot locate the cargo target directory.'
    );
  }
}

export function isDevRunTargetDirectory(targetDirectory) {
  return basename(resolve(targetDirectory)) === DEV_RUN_DIR_NAME;
}

/** The dev run's target directory under `targetDirectory`, or `targetDirectory` if it is one. */
export function devRunTargetDirectory(targetDirectory) {
  return isDevRunTargetDirectory(targetDirectory)
    ? targetDirectory
    : join(targetDirectory, DEV_RUN_DIR_NAME);
}

/** Builds `BIN_PACKAGES` into the current target directory, or exits non-zero with the likely causes. */
export function buildBinPackages(tool, { release }) {
  const args = [
    'build',
    ...(release ? ['--release'] : []),
    ...BIN_PACKAGES.flatMap((name) => ['-p', name]),
  ];
  try {
    execFileSync('cargo', args, { cwd: WORKSPACE_ROOT, stdio: 'inherit' });
  } catch {
    exitWith(
      tool,
      `cargo ${args.join(' ')} failed; see the cargo output above for the reason. If a running ` +
        'yagura-daemon holds one of these executables (os error 5 or 32 on Windows), first close ' +
        'any yagura GUI of that instance (an open GUI starts the daemon again; for the dev run, ' +
        'stop `npm run dev`), then stop the daemon with `yagura-daemon --shutdown` and re-run, ' +
        'or rebuild it in place with scripts/stash-rebuild.'
    );
  }
}
