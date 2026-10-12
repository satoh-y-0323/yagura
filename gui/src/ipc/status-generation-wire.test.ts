/**
 * The generation of a status pair crosses three wire shapes: the daemon's
 * window record, the daemon's status push, and the desktop layer's status
 * event. If any of them spelled the key differently, both ends would still
 * pass their own tests -- each builds its own fixtures -- while the real path
 * silently lost the generation and fell back to "always apply". So the key is
 * read out of the Rust sources here and used verbatim against this boundary,
 * rather than written twice.
 */
import { describe, it, expect } from 'vitest';

import protocolSource from '../../../crates/yagura-protocol/src/lib.rs?raw';
import desktopEventsSource from '../../src-tauri/src/events.rs?raw';
import { narrowStatusPayload, narrowWindowInfo } from './session-boundary';
import { itemBody } from '../test-utils/markup-scan';

/** Sample window id; the boundary treats it as an opaque string. */
const WINDOW_ID = 'w1-k3f9xa';

/**
 * The name of the one field a Rust item declares as `Option<u64>`. Found by
 * the field's shape rather than by its name, so this file never states the
 * name it is checking.
 */
function generationFieldName(source: string, header: string): string {
  const body = itemBody(source, header);
  const matches = [
    ...body.matchAll(/^[ \t]*(?:pub +)?([A-Za-z_][A-Za-z0-9_]*) *: *Option<u64> *,/gm),
  ];
  const names = matches.map((match) => match[1] ?? '');
  if (names.length !== 1) {
    throw new Error(
      `expected exactly one Option<u64> field under ${header}, found ${names.length}`
    );
  }
  return names[0] ?? '';
}

const WINDOW_RECORD_KEY = generationFieldName(protocolSource, 'pub struct WindowInfo {');
const STATUS_PUSH_KEY = generationFieldName(protocolSource, '    StatusChanged {');
const DESKTOP_EVENT_KEY = generationFieldName(desktopEventsSource, 'pub struct StatusPayload {');

describe('the three wire shapes name the status generation identically', () => {
  it('reads a non-empty source for each side (sanity check against a vacuous pass)', () => {
    expect(protocolSource.length).toBeGreaterThan(0);
    expect(desktopEventsSource.length).toBeGreaterThan(0);
  });

  it('finds a generation field in each of the three shapes', () => {
    expect(WINDOW_RECORD_KEY.length).toBeGreaterThan(0);
    expect(STATUS_PUSH_KEY.length).toBeGreaterThan(0);
    expect(DESKTOP_EVENT_KEY.length).toBeGreaterThan(0);
  });

  it('spells the generation key the same way in all three', () => {
    expect(new Set([WINDOW_RECORD_KEY, STATUS_PUSH_KEY, DESKTOP_EVENT_KEY]).size).toBe(1);
  });
});

describe('this boundary reads the generation under the key the Rust sources use', () => {
  it('carries the generation of a status event sent under the desktop layer key', () => {
    const payload = narrowStatusPayload({
      status: 'running',
      window_id: WINDOW_ID,
      [DESKTOP_EVENT_KEY]: 9,
    });
    expect(payload.status_rev).toBe(9);
  });

  it('carries the generation of a window record sent under the daemon record key', () => {
    const info = narrowWindowInfo({
      window_id: WINDOW_ID,
      name: 'main',
      vendor: null,
      cwd: '/x',
      status: 'running',
      exit_code: null,
      [WINDOW_RECORD_KEY]: 4,
    });
    expect(info.status_rev).toBe(4);
  });

  it('carries the generation of a status event sent under the daemon push key', () => {
    const payload = narrowStatusPayload({
      status: 'running',
      window_id: WINDOW_ID,
      [STATUS_PUSH_KEY]: 5,
    });
    expect(payload.status_rev).toBe(5);
  });

  it('drops a generation sent under a key none of the Rust shapes uses', () => {
    const misspelled = `${WINDOW_RECORD_KEY}ision`;
    const payload = narrowStatusPayload({
      status: 'running',
      window_id: WINDOW_ID,
      [misspelled]: 9,
    });
    expect(Object.prototype.hasOwnProperty.call(payload, 'status_rev')).toBe(false);
  });
});
