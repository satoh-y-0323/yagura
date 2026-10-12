/**
 * Whether a window shows the mark that says its CLI's status reports are
 * not arriving. Pure so {@link StatusReportMark.svelte} has no branching
 * logic of its own.
 */
import type { hints } from '../ui-messages';

/** What to show for a window whose status reports are known to be missing. */
export interface StatusReportMark {
  /** The `hints` key holding the tooltip / accessible-name text. */
  tooltipKey: keyof typeof hints;
}

/**
 * The mark to show for a window, `null` when nothing should show.
 *
 * Only a known gap (`true`) shows it: reports arriving or not expected
 * (`false`) and an unknown state (`null`) both show nothing.
 * @param statusReportsMissing whether the window's status reports are missing
 */
export function statusReportMarkOf(statusReportsMissing: boolean | null): StatusReportMark | null {
  return statusReportsMissing === true ? { tooltipKey: 'statusReportsMissing' } : null;
}
