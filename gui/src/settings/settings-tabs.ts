/** The tabs of the Settings panel, in the order the tab strip shows them. */
export const SETTINGS_TABS = ['profiles', 'approvals', 'integrations', 'keyboard'] as const;

/** One tab of the Settings panel. */
export type SettingsTab = (typeof SETTINGS_TABS)[number];
