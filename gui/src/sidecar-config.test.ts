/**
 * The bundle ships the holder and the MCP bridge beside the daemon: Tauri must
 * name all three as sidecars, and the staging script must put all three in
 * place, or a build resolves some and not the others.
 */
import { describe, it, expect } from 'vitest';

import tauriConfJson from '../src-tauri/tauri.conf.json?raw';
import prepareSidecarSource from '../scripts/prepare-sidecar.mjs?raw';

const SIDECAR_NAMES = ['yagura-daemon', 'yagura-holder', 'yagura-mcp'] as const;

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
  it('names the daemon, the holder and the MCP bridge as external binaries', () => {
    const externalBin = parseExternalBin(tauriConfJson);

    expect(externalBin).toEqual(
      expect.arrayContaining(SIDECAR_NAMES.map((name) => `binaries/${name}`))
    );
  });

  it('stages the daemon, the holder and the MCP bridge', () => {
    const code = codeOf(prepareSidecarSource);

    const missing = SIDECAR_NAMES.filter((name) => !namesInStringLiteral(code, name));

    expect(missing).toEqual([]);
  });
});
