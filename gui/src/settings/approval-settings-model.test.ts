/**
 * The pure model behind the Settings panel's Approvals tab: the rows it shows
 * for each gated tool and for the plan review gate, the one line that explains
 * why the switches are frozen, and the line a failed switch leaves behind.
 *
 * Expected wording is read from the i18n resource, never copied here, so these
 * tests pin which message each state picks rather than the message itself.
 */
import { describe, it, expect } from 'vitest';

import {
  approvalSettingsNotice,
  approvalToolRows,
  approvalUpdateFailure,
  isApprovalSettingsReadOnly,
  isApprovalSettingsSupported,
  planReviewRow,
  type ApprovalToolRow,
  type PlanReviewRow,
} from './approval-settings-model';
import { APPROVAL_SETTINGS_MIN_REVISION } from '../ipc/session-boundary';
import type { ApprovalSettingsInfo, PlaybookState } from '../ipc/session-client';
import { settings } from '../ui-messages';

/** The tools the daemon gates, as it names them on the wire. */
const GATED_TOOLS = [
  'open_window',
  'interrupt_window',
  'close_window',
  'send_message',
  'send_keys',
  'send_handoff',
  'send_chat',
  'broadcast_chat',
] as const;

/**
 * Reads one message from the resource and fails unless it is real text, so a
 * key that does not exist can never compare equal to a missing value.
 */
function text(value: unknown): string {
  expect(typeof value).toBe('string');
  expect((value as string).length).toBeGreaterThan(0);
  return value as string;
}

function info(overrides: Partial<ApprovalSettingsInfo> = {}): ApprovalSettingsInfo {
  return {
    playbook: 'loaded',
    tools: GATED_TOOLS.map((tool) => ({ tool, autoApprove: tool === 'send_chat' })),
    plansRequireApproval: true,
    ...overrides,
  };
}

describe('approvalToolRows', () => {
  it('keeps the order the daemon sent, not a fixed order of its own', () => {
    const order = [
      'send_chat',
      'broadcast_chat',
      'interrupt_window',
      'send_keys',
      'close_window',
      'send_message',
      'send_handoff',
    ];
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({ tools: order.map((tool) => ({ tool, autoApprove: false })) })
    );

    expect(rows.map((row) => row.id)).toEqual(order);
  });

  it('builds one row per tool the daemon sent and no more', () => {
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({ tools: [{ tool: 'send_keys', autoApprove: true }] })
    );

    expect(rows.map((row) => row.id)).toEqual(['send_keys']);
  });

  it('is empty when the daemon sent no tools', () => {
    const rows: ApprovalToolRow[] = approvalToolRows(info({ tools: [] }));

    expect(rows).toEqual([]);
  });

  it.each(GATED_TOOLS)('gives the known tool %s a name, a description and a risk', (tool) => {
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({ tools: [{ tool, autoApprove: false }] })
    );
    const row = rows[0];

    expect(row?.id).toBe(tool);
    expect(text(row?.name)).not.toBe(tool);
    text(row?.description);
    text(row?.risk);
  });

  it('describes every known tool in its own words', () => {
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({ tools: GATED_TOOLS.map((tool) => ({ tool, autoApprove: false })) })
    );

    expect(new Set(rows.map((row) => row.name)).size).toBe(GATED_TOOLS.length);
    expect(new Set(rows.map((row) => row.description)).size).toBe(GATED_TOOLS.length);
  });

  it.each(['a_newer_tool', 'toString', 'constructor', '__proto__'])(
    'shows the unknown tool %s by its id alone',
    (tool) => {
      const rows: ApprovalToolRow[] = approvalToolRows(
        info({ tools: [{ tool, autoApprove: true }] })
      );

      expect(rows).toEqual([
        {
          id: tool,
          name: tool,
          description: null,
          risk: null,
          autoApprove: true,
          stateLabel: text(settings.approvalRunsAutomatically),
          isRiskEmphasized: true,
        },
      ]);
    }
  );

  it('keeps an unknown tool in its place among known ones', () => {
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({
        tools: [
          { tool: 'send_keys', autoApprove: false },
          { tool: 'a_newer_tool', autoApprove: false },
          { tool: 'send_chat', autoApprove: false },
        ],
      })
    );

    expect(rows.map((row) => row.id)).toEqual(['send_keys', 'a_newer_tool', 'send_chat']);
    expect(rows[1]?.name).toBe('a_newer_tool');
  });

  it('labels a tool that runs automatically and emphasizes its risk', () => {
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({ tools: [{ tool: 'send_keys', autoApprove: true }] })
    );

    expect(rows[0]).toMatchObject({
      autoApprove: true,
      stateLabel: text(settings.approvalRunsAutomatically),
      isRiskEmphasized: true,
    });
  });

  it('labels a tool that waits for approval and does not emphasize its risk', () => {
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({ tools: [{ tool: 'send_keys', autoApprove: false }] })
    );

    expect(rows[0]).toMatchObject({
      autoApprove: false,
      stateLabel: text(settings.approvalWaitsForApproval),
      isRiskEmphasized: false,
    });
  });

  it('reads each row state from its own entry', () => {
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({
        tools: [
          { tool: 'send_message', autoApprove: true },
          { tool: 'close_window', autoApprove: false },
        ],
      })
    );

    expect(rows.map((row) => row.autoApprove)).toEqual([true, false]);
    expect(rows.map((row) => row.stateLabel)).toEqual([
      text(settings.approvalRunsAutomatically),
      text(settings.approvalWaitsForApproval),
    ]);
  });

  it('keeps the same name, description and risk whether the tool is on or off', () => {
    const [on] = approvalToolRows(info({ tools: [{ tool: 'send_keys', autoApprove: true }] }));
    const [off] = approvalToolRows(info({ tools: [{ tool: 'send_keys', autoApprove: false }] }));

    expect([on?.name, on?.description, on?.risk]).toEqual([off?.name, off?.description, off?.risk]);
  });
});

describe('planReviewRow', () => {
  it('reviews new plans first when the gate is on, with no risk line', () => {
    const row: PlanReviewRow = planReviewRow(info({ plansRequireApproval: true }));

    expect(row).toEqual({
      required: true,
      stateLabel: text(settings.planReviewFirst),
      risk: null,
    });
  });

  it('runs new plans without review when the gate is off, and says what can happen', () => {
    const row: PlanReviewRow = planReviewRow(info({ plansRequireApproval: false }));

    expect(row).toEqual({
      required: false,
      stateLabel: text(settings.planRunsWithoutReview),
      risk: text(settings.planReviewOffRisk),
    });
  });

  it('does not take its state from the tools', () => {
    const allAutomatic = GATED_TOOLS.map((tool) => ({ tool, autoApprove: true }));
    const row: PlanReviewRow = planReviewRow(
      info({ tools: allAutomatic, plansRequireApproval: true })
    );

    expect(row.required).toBe(true);
  });
});

describe('isApprovalSettingsSupported', () => {
  it('pins the first wire revision that carries the approval settings', () => {
    expect(APPROVAL_SETTINGS_MIN_REVISION).toBe(12);
  });

  it('is supported before the daemon has reported a revision', () => {
    expect(isApprovalSettingsSupported(null)).toBe(true);
  });

  it('is not supported one revision short', () => {
    expect(isApprovalSettingsSupported(11)).toBe(false);
  });

  it('is supported from that revision onward', () => {
    expect(isApprovalSettingsSupported(12)).toBe(true);
    expect(isApprovalSettingsSupported(13)).toBe(true);
  });
});

describe('approvalSettingsNotice', () => {
  it('asks for a newer daemon when the daemon predates the settings', () => {
    const notice: string | null = approvalSettingsNotice(null, 11);

    expect(notice).toBe(text(settings.approvalSettingsNeedNewerDaemon));
  });

  it('asks for a newer daemon even if settings are somehow at hand', () => {
    const notice: string | null = approvalSettingsNotice(info({ playbook: 'loaded' }), 11);

    expect(notice).toBe(text(settings.approvalSettingsNeedNewerDaemon));
  });

  it.each<[PlaybookState, () => string]>([
    ['failed', () => text(settings.approvalPlaybookFailed)],
    ['not_configured', () => text(settings.approvalPlaybookNotConfigured)],
    ['unknown', () => text(settings.approvalPlaybookUnknown)],
  ])('explains the %s playbook state', (playbook, expected) => {
    const notice: string | null = approvalSettingsNotice(info({ playbook }), 12);

    expect(notice).toBe(expected());
  });

  it('explains the playbook state when the revision is not reported', () => {
    const notice: string | null = approvalSettingsNotice(info({ playbook: 'failed' }), null);

    expect(notice).toBe(text(settings.approvalPlaybookFailed));
  });

  it('shows nothing for a loaded playbook', () => {
    expect(approvalSettingsNotice(info({ playbook: 'loaded' }), 12)).toBeNull();
    expect(approvalSettingsNotice(info({ playbook: 'loaded' }), null)).toBeNull();
  });

  it('shows nothing before the settings have been read', () => {
    expect(approvalSettingsNotice(null, 12)).toBeNull();
    expect(approvalSettingsNotice(null, null)).toBeNull();
  });
});

describe('isApprovalSettingsReadOnly', () => {
  it('is editable for a loaded playbook', () => {
    expect(isApprovalSettingsReadOnly(info({ playbook: 'loaded' }), 12)).toBe(false);
    expect(isApprovalSettingsReadOnly(info({ playbook: 'loaded' }), null)).toBe(false);
  });

  it.each<PlaybookState>(['failed', 'not_configured', 'unknown'])(
    'is read-only for the %s playbook state',
    (playbook) => {
      expect(isApprovalSettingsReadOnly(info({ playbook }), 12)).toBe(true);
    }
  );

  it('is read-only for a daemon that predates the settings', () => {
    expect(isApprovalSettingsReadOnly(info({ playbook: 'loaded' }), 11)).toBe(true);
  });

  it('is read-only before the settings have been read', () => {
    expect(isApprovalSettingsReadOnly(null, 12)).toBe(true);
    expect(isApprovalSettingsReadOnly(null, null)).toBe(true);
  });
});

describe('approvalUpdateFailure', () => {
  it.each<[string, boolean, 'tool' | 'plan']>([
    ['send_keys', true, 'tool'],
    ['send_keys', false, 'tool'],
    ['require_approval', true, 'plan'],
    ['require_approval', false, 'plan'],
  ])('names %s and the reason (was on: %s, %s)', (id, wasOn, kind) => {
    const reason = 'the playbook could not be written';
    const line: string = approvalUpdateFailure(id, reason, wasOn, kind);

    expect(line).toContain(id);
    expect(line).toContain(reason);
  });

  it('gives each of the four cases its own line', () => {
    const reason = 'refused';
    const lines = [
      approvalUpdateFailure('send_keys', reason, true, 'tool'),
      approvalUpdateFailure('send_keys', reason, false, 'tool'),
      approvalUpdateFailure('send_keys', reason, true, 'plan'),
      approvalUpdateFailure('send_keys', reason, false, 'plan'),
    ];

    expect(new Set(lines).size).toBe(lines.length);
  });
});
