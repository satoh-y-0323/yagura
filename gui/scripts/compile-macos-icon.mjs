#!/usr/bin/env node
// Compiles the macOS 26 icon (`icons/yagura.icon`) into the `Assets.car` that
// `tauri.macos.conf.json` lists in `bundle.icon`. Tauri can compile a `.icon`
// itself, but it starts actool without a stdin under the Node.js CLI and actool
// crashes (tauri-apps/tauri#15315), so actool runs here with stdin on /dev/null.
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const srcTauri = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'src-tauri');
const iconSource = join(srcTauri, 'icons', 'yagura.icon');
const outDir = join(srcTauri, 'gen', 'macos-icon');

const work = mkdtempSync(join(tmpdir(), 'yagura-icon-'));
try {
  execFileSync(
    'xcrun',
    [
      'actool',
      iconSource,
      '--compile',
      work,
      '--output-format',
      'human-readable-text',
      '--notices',
      '--warnings',
      '--output-partial-info-plist',
      join(work, 'partial-info.plist'),
      '--app-icon',
      'yagura',
      '--include-all-app-icons',
      '--target-device',
      'mac',
      '--minimum-deployment-target',
      '26.0',
      '--platform',
      'macosx',
    ],
    { stdio: ['ignore', 'inherit', 'inherit'] }
  );
  const car = join(work, 'Assets.car');
  if (!existsSync(car)) {
    console.error('compile-macos-icon: actool wrote no Assets.car (needs Xcode 26 or later)');
    process.exit(1);
  }
  mkdirSync(outDir, { recursive: true });
  copyFileSync(car, join(outDir, 'Assets.car'));
} finally {
  rmSync(work, { recursive: true, force: true });
}
