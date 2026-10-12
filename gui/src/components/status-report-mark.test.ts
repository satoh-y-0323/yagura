/**
 * When a window shows the mark that says its CLI's status reports are not
 * arriving. Only a known gap shows it: no gap and an unknown state show
 * nothing, so the view never warns about something it has not seen.
 */
import { describe, it, expect } from 'vitest';

import { hints } from '../ui-messages';

// Loaded per case so each case fails on its own while the module is missing.
async function loadStatusReportMarkOf(): Promise<
  (typeof import('./status-report-mark'))['statusReportMarkOf']
> {
  const module = await import('./status-report-mark');
  return module.statusReportMarkOf;
}

describe('statusReportMarkOf', () => {
  it('shows the mark when the reports are missing', async () => {
    const statusReportMarkOf = await loadStatusReportMarkOf();

    expect(statusReportMarkOf(true)).not.toBeNull();
  });

  it('names a tooltip that says the reports are not arriving', async () => {
    const statusReportMarkOf = await loadStatusReportMarkOf();

    const mark = statusReportMarkOf(true);

    expect(mark === null ? null : hints[mark.tooltipKey]).toBe(
      "Status reports from this CLI aren't arriving. yagura is reading the screen only."
    );
  });

  it('shows nothing when the reports are arriving or not expected', async () => {
    const statusReportMarkOf = await loadStatusReportMarkOf();

    expect(statusReportMarkOf(false)).toBeNull();
  });

  it('shows nothing when the state is unknown', async () => {
    const statusReportMarkOf = await loadStatusReportMarkOf();

    expect(statusReportMarkOf(null)).toBeNull();
  });
});
