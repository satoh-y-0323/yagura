#!/usr/bin/env node
// The distribution entry point. The base config refuses to bundle (see refuse-bundle.mjs), so the
// app is built without bundling and then bundled with this OS's distribution config, which lifts
// that refusal and carries everything the bundle ships beside the app.
import { join } from 'node:path';
import { GUI_DIR, exitWith, refuseArguments, runTauri } from './workspace-bins.mjs';

const TOOL = 'build-bundle';

const BUNDLE_CONFIGS = {
  win32: 'tauri.bundle.windows.conf.json',
  linux: 'tauri.bundle.linux.conf.json',
  darwin: 'tauri.bundle.macos.conf.json',
};

// prepare-sidecar stages the sidecars for the host triple only, so no other target can be bundled.
refuseArguments(
  TOOL,
  'a distribution is built for the host triple only, the one the sidecars are staged for.'
);

const configName = BUNDLE_CONFIGS[process.platform];
if (configName === undefined) {
  exitWith(TOOL, `no distribution config for the platform ${process.platform}.`);
}
const bundleConfig = join(GUI_DIR, 'src-tauri', configName);

runTauri(TOOL, 'tauri build --no-bundle', ['build', '--no-bundle'], { reportFailure: true });
runTauri(TOOL, `tauri bundle --config ${configName}`, ['bundle', '--config', bundleConfig], {
  reportFailure: true,
});
