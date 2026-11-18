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

/** The key this boundary reads the status generation under. */
const GENERATION_KEY = 'status_rev';

/** The key this boundary reads the boot the generation was counted in under. */
const BOOT_KEY = 'status_boot';

/**
 * The name of the `Option<rustType>` field a Rust item declares under `key`.
 * A record may carry other fields of the same type (a window record also
 * counts its waiting messages in an `Option<u64>`), so the field is found by
 * its name and its shape together.
 */
function generationFieldName(
  source: string,
  header: string,
  key: string,
  rustType: string
): string {
  const body = itemBody(source, header);
  const shape = new RegExp(
    `^[ \\t]*(?:pub +)?([A-Za-z_][A-Za-z0-9_]*) *: *Option<${rustType}> *,`,
    'gm'
  );
  const names = [...body.matchAll(shape)]
    .map((match) => match[1] ?? '')
    .filter((name) => name === key);
  if (names.length !== 1) {
    throw new Error(
      `expected exactly one Option<${rustType}> field named ${key} under ${header}, found ${names.length}`
    );
  }
  return names[0] ?? '';
}

const WINDOW_RECORD_KEY = generationFieldName(
  protocolSource,
  'pub struct WindowInfo {',
  GENERATION_KEY,
  'u64'
);
const STATUS_PUSH_KEY = generationFieldName(
  protocolSource,
  '    StatusChanged {',
  GENERATION_KEY,
  'u64'
);
const DESKTOP_EVENT_KEY = generationFieldName(
  desktopEventsSource,
  'pub struct StatusPayload {',
  GENERATION_KEY,
  'u64'
);
const WINDOW_RECORD_BOOT_KEY = generationFieldName(
  protocolSource,
  'pub struct WindowInfo {',
  BOOT_KEY,
  'StatusBoot'
);
const STATUS_PUSH_BOOT_KEY = generationFieldName(
  protocolSource,
  '    StatusChanged {',
  BOOT_KEY,
  'StatusBoot'
);
const DESKTOP_EVENT_BOOT_KEY = generationFieldName(
  desktopEventsSource,
  'pub struct StatusPayload {',
  BOOT_KEY,
  'StatusBoot'
);

/** The key this boundary reads a task window's report mark under. */
const TASK_REPORT_KEY = 'task_report';

const WINDOW_RECORD_TASK_REPORT_KEY = generationFieldName(
  protocolSource,
  'pub struct WindowInfo {',
  TASK_REPORT_KEY,
  'TaskReportMark'
);
const STATUS_PUSH_TASK_REPORT_KEY = generationFieldName(
  protocolSource,
  '    StatusChanged {',
  TASK_REPORT_KEY,
  'TaskReportMark'
);
const DESKTOP_EVENT_TASK_REPORT_KEY = generationFieldName(
  desktopEventsSource,
  'pub struct StatusPayload {',
  TASK_REPORT_KEY,
  'TaskReportMark'
);

/** The one field of `rustType` the Rust `TaskReportMark` declares. */
function markFieldName(rustType: string): string {
  const body = itemBody(protocolSource, 'pub struct TaskReportMark {');
  const shape = new RegExp(`^[ \\t]*pub +([A-Za-z_][A-Za-z0-9_]*) *: *${rustType} *,`, 'gm');
  const names = [...body.matchAll(shape)].map((match) => match[1] ?? '');
  if (names.length !== 1) {
    throw new Error(
      `expected exactly one ${rustType} field in TaskReportMark, found ${names.length}`
    );
  }
  return names[0] ?? '';
}

/** A report mark spelled with the Rust field names. */
function rustSpelledMark(seq: number, state: string): Record<string, unknown> {
  return { [markFieldName('u64')]: seq, [markFieldName('ReportState')]: state };
}

/** The `#[...]` attribute lines directly above the item at `index`, doc lines skipped. */
function attributesAbove(source: string, index: number): string {
  const lines = source.slice(0, index).split('\n').slice(0, -1);
  const attributes: string[] = [];
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = (lines[i] ?? '').trim();
    if (line.startsWith('#[')) {
      attributes.push(line);
    } else if (!line.startsWith('///')) {
      break;
    }
  }
  return attributes.join('\n');
}

/** A variant name as serde writes it under the enum's `rename_all` rule. */
function spellVariant(rule: string | null, name: string): string {
  if (rule === null) {
    return name;
  }
  if (rule === 'snake_case') {
    return name.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();
  }
  throw new Error(`this reader does not know the rename_all rule ${rule}`);
}

/**
 * The wire spellings of the Rust `ReportState`, read from its declaration: every
 * unit variant under the enum's `rename_all` rule (or its own `rename`), leaving
 * out the `#[serde(other)]` catch-all a peer's unknown value decodes to.
 */
function rustReportStateSpellings(): string[] {
  const header = 'pub enum ReportState {';
  const at = protocolSource.indexOf(header);
  if (at === -1) {
    throw new Error(`no Rust item found for header: ${header}`);
  }
  const rule = /rename_all *= *"([A-Za-z_]+)"/.exec(attributesAbove(protocolSource, at))?.[1];
  const spellings: string[] = [];
  let variantAttributes = '';
  for (const raw of itemBody(protocolSource, header).split('\n')) {
    const line = raw.trim();
    if (line === '' || line.startsWith('///')) {
      continue;
    }
    if (line.startsWith('#[')) {
      variantAttributes += line;
      continue;
    }
    const variant = /^([A-Z][A-Za-z0-9]*) *,?$/.exec(line);
    if (variant === null) {
      throw new Error(`ReportState holds a line this reader cannot read: ${line}`);
    }
    if (!/serde\( *other *\)/.test(variantAttributes)) {
      const renamed = /serde\(rename *= *"([^"]*)"\)/.exec(variantAttributes)?.[1];
      spellings.push(renamed ?? spellVariant(rule ?? null, variant[1] ?? ''));
    }
    variantAttributes = '';
  }
  return spellings;
}

describe('this boundary reads the generation under the key the Rust sources use', () => {
  it('carries the generation of a status event sent under the desktop layer key and under the daemon push key', () => {
    const routes: [string, string, number][] = [
      [DESKTOP_EVENT_KEY, DESKTOP_EVENT_BOOT_KEY, 9],
      [STATUS_PUSH_KEY, STATUS_PUSH_BOOT_KEY, 5],
    ];
    for (const [key, bootKey, generation] of routes) {
      const payload = narrowStatusPayload({
        status: 'running',
        window_id: WINDOW_ID,
        [key]: generation,
        [bootKey]: { origin: 7, seq: 3 },
      });
      expect(payload.status_rev, key).toBe(generation);
      expect(payload.status_boot, key).toEqual({ origin: 7, seq: 3 });
    }
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
      [WINDOW_RECORD_BOOT_KEY]: { origin: 7, seq: 3 },
    });
    expect(info.status_rev).toBe(4);
    expect(info.status_boot).toEqual({ origin: 7, seq: 3 });
  });

  it('carries the report mark of each shape sent under the Rust key and field names', () => {
    const desktop = narrowStatusPayload({
      status: 'waiting_input',
      window_id: WINDOW_ID,
      [DESKTOP_EVENT_TASK_REPORT_KEY]: rustSpelledMark(2, 'standing'),
    });
    expect(desktop.task_report).toEqual({ seq: 2, state: 'standing' });

    const push = narrowStatusPayload({
      status: 'waiting_input',
      window_id: WINDOW_ID,
      [STATUS_PUSH_TASK_REPORT_KEY]: rustSpelledMark(3, 'instruction'),
    });
    expect(push.task_report).toEqual({ seq: 3, state: 'instruction' });

    const record = narrowWindowInfo({
      window_id: WINDOW_ID,
      name: 'main',
      vendor: null,
      cwd: '/x',
      status: 'waiting_input',
      exit_code: null,
      [WINDOW_RECORD_TASK_REPORT_KEY]: rustSpelledMark(1, 'standing'),
    });
    expect(record.task_report).toEqual({ seq: 1, state: 'standing' });
  });

  it('finds the three report states the daemon writes in the Rust ReportState, its catch-all left out', () => {
    expect(rustReportStateSpellings()).toHaveLength(3);
  });

  it('reads every report state the Rust ReportState writes as itself on both routes, never as unknown', () => {
    for (const state of rustReportStateSpellings()) {
      const push = narrowStatusPayload({
        status: 'waiting_input',
        window_id: WINDOW_ID,
        [STATUS_PUSH_TASK_REPORT_KEY]: rustSpelledMark(4, state),
      });
      expect(push.task_report, state).toEqual({ seq: 4, state });

      const record = narrowWindowInfo({
        window_id: WINDOW_ID,
        name: 'main',
        vendor: null,
        cwd: '/x',
        status: 'waiting_input',
        exit_code: null,
        [WINDOW_RECORD_TASK_REPORT_KEY]: rustSpelledMark(4, state),
      });
      expect(record.task_report, state).toEqual({ seq: 4, state });
    }
  });

  it('drops a generation sent under a key none of the Rust shapes uses', () => {
    const misspelled = `${WINDOW_RECORD_KEY}ision`;
    const misspelledBoot = `${WINDOW_RECORD_BOOT_KEY}ision`;
    const payload = narrowStatusPayload({
      status: 'running',
      window_id: WINDOW_ID,
      [misspelled]: 9,
      [misspelledBoot]: { origin: 7, seq: 3 },
    });
    expect(Object.prototype.hasOwnProperty.call(payload, 'status_rev')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(payload, 'status_boot')).toBe(false);

    const misspelledMark = narrowStatusPayload({
      status: 'waiting_input',
      window_id: WINDOW_ID,
      [`${STATUS_PUSH_TASK_REPORT_KEY}s`]: rustSpelledMark(1, 'standing'),
    });
    expect(Object.prototype.hasOwnProperty.call(misspelledMark, 'task_report')).toBe(false);
  });
});
