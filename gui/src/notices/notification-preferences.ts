import { NOTICE_CATEGORIES, type NoticeCategory } from './notice-model';

/** The `localStorage` key the preferences are kept under. */
export const NOTIFICATION_PREFERENCES_KEY = 'yagura.notification-preferences';

const FORMAT_VERSION = 1;

/** Which notice kinds the user lets appear on the notification bar, one switch per settings row. */
export interface NotificationPreferences {
  inApp: Readonly<Record<NoticeCategory, boolean>>;
}

function allOn(): Record<NoticeCategory, boolean> {
  return Object.fromEntries(NOTICE_CATEGORIES.map((row) => [row, true])) as Record<
    NoticeCategory,
    boolean
  >;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Reads the stored preferences. Anything that cannot be read as the current
 * format — nothing stored, broken JSON, another version, a value that is not
 * a boolean — reads as on, so a lost setting never silences a notice. Keys it
 * does not know are dropped.
 * @param raw the stored text, `null` when nothing is stored
 */
export function readPreferences(raw: string | null): NotificationPreferences {
  const inApp = allOn();
  if (raw === null) {
    return { inApp };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { inApp };
  }
  if (!isRecord(parsed) || parsed.v !== FORMAT_VERSION || !isRecord(parsed.inApp)) {
    return { inApp };
  }
  const stored = parsed.inApp;
  for (const row of NOTICE_CATEGORIES) {
    const value = stored[row];
    if (typeof value === 'boolean') {
      inApp[row] = value;
    }
  }
  return { inApp };
}

/**
 * The text to store for the preferences: the format version and only the
 * rows it knows.
 * @param prefs the preferences to store
 */
export function writePreferences(prefs: NotificationPreferences): string {
  const inApp = Object.fromEntries(NOTICE_CATEGORIES.map((row) => [row, prefs.inApp[row]]));
  return JSON.stringify({ v: FORMAT_VERSION, inApp });
}

/**
 * New preferences with one row switched; the ones given are left unchanged.
 * @param prefs the current preferences
 * @param category the row to switch
 * @param enabled whether that kind may appear on the notification bar
 */
export function withPreference(
  prefs: NotificationPreferences,
  category: NoticeCategory,
  enabled: boolean
): NotificationPreferences {
  return { inApp: { ...prefs.inApp, [category]: enabled } };
}
