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
  planReviewRow,
  type ApprovalToolRow,
  type PlanReviewRow,
} from './approval-settings-model';
import type { ApprovalSettingsInfo, PlaybookState } from '../ipc/session-client';
import { settings } from '../ui-messages';

/** The tools the daemon gates, as it names them on the wire. */
const GATED_TOOLS = [
  'open_window',
  'interrupt_window',
  'close_window',
  'send_message',
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
      info({ tools: [{ tool: 'interrupt_window', autoApprove: true }] })
    );

    expect(rows.map((row) => row.id)).toEqual(['interrupt_window']);
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
          { tool: 'interrupt_window', autoApprove: false },
          { tool: 'a_newer_tool', autoApprove: false },
          { tool: 'send_chat', autoApprove: false },
        ],
      })
    );

    expect(rows.map((row) => row.id)).toEqual(['interrupt_window', 'a_newer_tool', 'send_chat']);
    expect(rows[1]?.name).toBe('a_newer_tool');
  });

  it('labels a tool that runs automatically and emphasizes its risk', () => {
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({ tools: [{ tool: 'interrupt_window', autoApprove: true }] })
    );

    expect(rows[0]).toMatchObject({
      autoApprove: true,
      stateLabel: text(settings.approvalRunsAutomatically),
      isRiskEmphasized: true,
    });
  });

  it('labels a tool that waits for approval and does not emphasize its risk', () => {
    const rows: ApprovalToolRow[] = approvalToolRows(
      info({ tools: [{ tool: 'interrupt_window', autoApprove: false }] })
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
    const [on] = approvalToolRows(
      info({ tools: [{ tool: 'interrupt_window', autoApprove: true }] })
    );
    const [off] = approvalToolRows(
      info({ tools: [{ tool: 'interrupt_window', autoApprove: false }] })
    );

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

describe('approvalSettingsNotice', () => {
  it.each<[PlaybookState, () => string]>([
    ['failed', () => text(settings.approvalPlaybookFailed)],
    ['not_configured', () => text(settings.approvalPlaybookNotConfigured)],
    ['unknown', () => text(settings.approvalPlaybookUnknown)],
  ])('explains the %s playbook state', (playbook, expected) => {
    const notice: string | null = approvalSettingsNotice(info({ playbook }));

    expect(notice).toBe(expected());
  });

  it('shows nothing for a loaded playbook', () => {
    expect(approvalSettingsNotice(info({ playbook: 'loaded' }))).toBeNull();
  });

  it('shows nothing before the settings have been read', () => {
    expect(approvalSettingsNotice(null)).toBeNull();
  });
});

describe('isApprovalSettingsReadOnly', () => {
  it('is editable for a loaded playbook', () => {
    expect(isApprovalSettingsReadOnly(info({ playbook: 'loaded' }))).toBe(false);
  });

  it.each<PlaybookState>(['failed', 'not_configured', 'unknown'])(
    'is read-only for the %s playbook state',
    (playbook) => {
      expect(isApprovalSettingsReadOnly(info({ playbook }))).toBe(true);
    }
  );

  it('is read-only before the settings have been read', () => {
    expect(isApprovalSettingsReadOnly(null)).toBe(true);
  });
});

describe('approvalUpdateFailure', () => {
  it.each<[string, boolean, 'tool' | 'plan']>([
    ['interrupt_window', true, 'tool'],
    ['interrupt_window', false, 'tool'],
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
      approvalUpdateFailure('interrupt_window', reason, true, 'tool'),
      approvalUpdateFailure('interrupt_window', reason, false, 'tool'),
      approvalUpdateFailure('interrupt_window', reason, true, 'plan'),
      approvalUpdateFailure('interrupt_window', reason, false, 'plan'),
    ];

    expect(new Set(lines).size).toBe(lines.length);
  });
});

/**
 * The Integrations tab's state line for a registration whose arguments differ
 * (`differs_in` names `args`): its own English phrase, not the per-key
 * template, which would print the wire token as a word.
 */
describe('the registration state that names differing arguments', () => {
  it('words differing arguments in a phrase of their own', () => {
    const templated = settings.integrationDiffersIn('some_key').replace('some_key', 'args');

    expect(settings.integrationDiffersIn('args')).not.toBe(templated);
    expect(settings.integrationDiffersIn('args')).not.toMatch(/\bargs\b/);
  });
});
