/**
 * The bundle ships the holder and the MCP bridge beside the daemon, and on
 * Linux and macOS the watcher each holder starts: Tauri must name every one of
 * them as a sidecar on the platforms that ship it, and the staging script must
 * put them in place, or a build resolves some and not the others.
 */
import { describe, it, expect } from 'vitest';

import tauriConfJson from '../src-tauri/tauri.conf.json?raw';
import tauriLinuxConfJson from '../src-tauri/tauri.linux.conf.json?raw';
import tauriMacosConfJson from '../src-tauri/tauri.macos.conf.json?raw';
import prepareSidecarSource from '../scripts/prepare-sidecar.mjs?raw';

const SIDECAR_NAMES = ['yagura-daemon', 'yagura-holder', 'yagura-mcp'] as const;
const WATCHER_NAME = 'yagura-watcher';

/** The `bundle.externalBin` array of the parsed Tauri config, validated as string[]. */
function parseExternalBin(raw: string): string[] {
  const parsed: unknown = JSON.parse(raw);
  const bundle =
    typeof parsed === 'object' && parsed !== null && 'bundle' in parsed ? parsed.bundle : undefined;
  const externalBin =
    typeof bundle === 'object' && bundle !== null && 'externalBin' in bundle
      ? bundle.externalBin
      : undefined;
  if (!Array.isArray(externalBin) || !externalBin.every((entry) => typeof entry === 'string')) {
    throw new Error('tauri.conf.json bundle.externalBin is not a string array');
  }
  return externalBin;
}

/** The script without its comments, so a name mentioned only in prose does not count. */
function codeOf(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('//'))
    .join('\n');
}

/** Whether `code` holds `name` as the start of a string literal's text. */
function namesInStringLiteral(code: string, name: string): boolean {
  return new RegExp(`[\`'"]${name}(?![\\w-])`).test(code);
}

describe('sidecar configuration', () => {
  it('names the daemon, the holder and the MCP bridge as external binaries, without the watcher by default', () => {
    const externalBin = parseExternalBin(tauriConfJson);

    expect(externalBin).toEqual(
      expect.arrayContaining(SIDECAR_NAMES.map((name) => `binaries/${name}`))
    );
    expect(externalBin).not.toContain(`binaries/${WATCHER_NAME}`);
  });

  // A platform's externalBin replaces the default list whole (JSON Merge Patch).
  it.each([
    ['Linux', tauriLinuxConfJson],
    ['macOS', tauriMacosConfJson],
  ])('names the watcher beside every other sidecar on %s', (_platform, raw) => {
    const externalBin = parseExternalBin(raw);

    expect(externalBin).toEqual(
      expect.arrayContaining([...SIDECAR_NAMES, WATCHER_NAME].map((name) => `binaries/${name}`))
    );
  });

  it('stages the daemon, the holder, the watcher and the MCP bridge', () => {
    const code = codeOf(prepareSidecarSource);

    const missing = [...SIDECAR_NAMES, WATCHER_NAME].filter(
      (name) => !namesInStringLiteral(code, name)
    );

    expect(missing).toEqual([]);
  });
});
