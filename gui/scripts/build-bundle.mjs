#!/usr/bin/env node
// The distribution entry point. The base config refuses to bundle (see refuse-bundle.mjs), so the
// app is built without bundling and then bundled with this OS's distribution config, which lifts
// that refusal and carries everything the bundle ships beside the app.
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const guiDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const tauriCli = join(guiDir, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');

const BUNDLE_CONFIGS = {
  win32: 'tauri.bundle.windows.conf.json',
  linux: 'tauri.bundle.linux.conf.json',
  darwin: 'tauri.bundle.macos.conf.json',
};

function fail(message, code = 1) {
  console.error(`build-bundle: ${message}`);
  process.exit(code);
}

// prepare-sidecar stages the sidecars for the host triple only, so no other target can be bundled.
if (process.argv.length > 2) {
  fail(
    `takes no arguments (got: ${process.argv.slice(2).join(' ')}); a distribution is built ` +
      'for the host triple only, the one the sidecars are staged for.'
  );
}

const configName = BUNDLE_CONFIGS[process.platform];
if (configName === undefined) {
  fail(`no distribution config for the platform ${process.platform}.`);
}
const bundleConfig = join(guiDir, 'src-tauri', configName);

function runStage(stage, args) {
  const result = spawnSync(process.execPath, [tauriCli, ...args], {
    cwd: guiDir,
    stdio: 'inherit',
  });
  if (result.error) {
    fail(`${stage} could not start: ${result.error.message}`);
  }
  if (result.status !== 0) {
    const how = result.status === null ? `signal ${result.signal}` : `exit code ${result.status}`;
    fail(`${stage} failed with ${how}.`, result.status ?? 1);
  }
}

runStage('tauri build --no-bundle', ['build', '--no-bundle']);
runStage(`tauri bundle --config ${configName}`, ['bundle', '--config', bundleConfig]);
