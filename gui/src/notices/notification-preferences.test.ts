import { describe, it, expect } from 'vitest';

import type { NoticeCategory } from './notice-model';
import type { NotificationPreferences } from './notification-preferences';

// `satisfies` makes the compiler hold this list to exactly the rows `NoticeCategory` names.
const ROW_SET = {
  'needs-approval': true,
  'needs-input': true,
  'window-exits': true,
  reconnected: true,
  'failed-actions': true,
  'setup-problems': true,
  'last-session': true,
} satisfies Record<NoticeCategory, true>;
const ROWS = Object.keys(ROW_SET) as NoticeCategory[];

const ALL_ON: NotificationPreferences = {
  inApp: Object.fromEntries(ROWS.map((row) => [row, true])) as Record<NoticeCategory, boolean>,
};

function allOnExcept(row: NoticeCategory): NotificationPreferences {
  return { inApp: { ...ALL_ON.inApp, [row]: false } };
}

async function load() {
  return await import('./notification-preferences');
}

describe('readPreferences', () => {
  it.each([
    ['nothing stored', null],
    ['an empty string', ''],
    ['text that is not JSON', 'not json'],
    ['truncated JSON', '{"v":1,"inApp":{'],
    ['a JSON array', '[]'],
    ['JSON null', 'null'],
    ['a JSON number', '42'],
    ['a JSON string', '"x"'],
    ['an object with neither key', '{}'],
  ])('reads %s as every row on', async (_name, raw) => {
    const { readPreferences } = await load();
    expect(readPreferences(raw)).toEqual(ALL_ON);
  });

  it('has exactly the seven rows, each on by default', async () => {
    const { readPreferences } = await load();
    const prefs = readPreferences(null);
    expect(Object.keys(prefs.inApp).sort()).toEqual([...ROWS].sort());
    expect(Object.values(prefs.inApp).every((on) => on === true)).toBe(true);
  });

  it('reads every row on when the version differs', async () => {
    const { readPreferences } = await load();
    const raw = JSON.stringify({ v: 2, inApp: { 'needs-input': false } });
    expect(readPreferences(raw)).toEqual(ALL_ON);
  });

  it('reads every row on when the version is missing', async () => {
    const { readPreferences } = await load();
    const raw = JSON.stringify({ inApp: { 'needs-input': false } });
    expect(readPreferences(raw)).toEqual(ALL_ON);
  });

  it.each([
    ['an array', [false, false]],
    ['null', null],
    ['a string', 'off'],
    ['a number', 0],
  ])('reads every row on when inApp is %s', async (_name, inApp) => {
    const { readPreferences } = await load();
    expect(readPreferences(JSON.stringify({ v: 1, inApp }))).toEqual(ALL_ON);
  });

  it('reads a value that is not a boolean as on and keeps the rows that are', async () => {
    const { readPreferences } = await load();
    const raw = JSON.stringify({
      v: 1,
      inApp: {
        'needs-input': 0,
        'needs-approval': 'false',
        reconnected: null,
        'failed-actions': {},
        'window-exits': false,
      },
    });
    expect(readPreferences(raw)).toEqual(allOnExcept('window-exits'));
  });

  it('reads a row that is missing as on', async () => {
    const { readPreferences } = await load();
    const raw = JSON.stringify({ v: 1, inApp: { 'last-session': false } });
    expect(readPreferences(raw)).toEqual(allOnExcept('last-session'));
  });

  it('drops keys it does not know, at both levels', async () => {
    const { readPreferences } = await load();
    const raw = JSON.stringify({
      v: 1,
      desktop: { 'needs-input': false },
      inApp: { 'needs-input': false, 'permission-requests': false },
    });
    expect(readPreferences(raw)).toEqual(allOnExcept('needs-input'));
  });
});

describe('writePreferences', () => {
  it.each(ROWS)('reads back what it wrote, with only %s off', async (row) => {
    const { readPreferences, writePreferences } = await load();
    const prefs = allOnExcept(row);
    expect(readPreferences(writePreferences(prefs))).toEqual(prefs);
  });

  it('writes the version and the seven rows under inApp', async () => {
    const { writePreferences } = await load();
    const written: unknown = JSON.parse(writePreferences(allOnExcept('needs-approval')));
    expect(written).toEqual({ v: 1, inApp: { ...ALL_ON.inApp, 'needs-approval': false } });
  });

  it('writes only the keys it knows', async () => {
    const { writePreferences } = await load();
    const withExtras = {
      inApp: { ...ALL_ON.inApp, 'permission-requests': false },
      desktop: { 'needs-input': false },
    } as unknown as NotificationPreferences;
    const written = JSON.parse(writePreferences(withExtras)) as {
      inApp: Record<string, unknown>;
    };
    expect(Object.keys(written).sort()).toEqual(['inApp', 'v']);
    expect(Object.keys(written.inApp).sort()).toEqual([...ROWS].sort());
  });
});

describe('withPreference', () => {
  it.each(ROWS)('turns only %s off and leaves the rest', async (row) => {
    const { withPreference } = await load();
    expect(withPreference(ALL_ON, row, false)).toEqual(allOnExcept(row));
  });

  it('turns a row back on', async () => {
    const { withPreference } = await load();
    expect(withPreference(allOnExcept('needs-input'), 'needs-input', true)).toEqual(ALL_ON);
  });

  it('keeps the other rows that are off', async () => {
    const { withPreference } = await load();
    const result = withPreference(allOnExcept('reconnected'), 'needs-input', false);
    expect(result.inApp.reconnected).toBe(false);
    expect(result.inApp['needs-input']).toBe(false);
    expect(Object.values(result.inApp).filter((on) => !on)).toHaveLength(2);
  });

  it('does not change the preferences it was given', async () => {
    const { withPreference } = await load();
    const before = allOnExcept('last-session');
    const snapshot = JSON.stringify(before);
    withPreference(before, 'needs-approval', false);
    expect(JSON.stringify(before)).toBe(snapshot);
  });
});
