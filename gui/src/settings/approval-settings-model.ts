/**
 * Pure decisions behind the Settings panel's Approvals tab: the rows it shows
 * for each gated tool and for the plan review gate, the one line that
 * explains why the switches are frozen, and the line a failed switch leaves
 * behind. No IPC, no DOM: the component renders these values and hands the
 * switch back to its caller.
 */
import { APPROVAL_SETTINGS_MIN_REVISION } from '../ipc/session-boundary';
import type { ApprovalSettingsInfo } from '../ipc/session-client';
import { settings } from '../ui-messages';

/** One tool's known display text, keyed by its wire name in `settings.approvalToolInfo`. */
type KnownApprovalTool = keyof typeof settings.approvalToolInfo;

/** The playbook key of the plan gate, shown as the plan row's id: the gate has no per-item id on the wire. */
export const PLAN_REVIEW_GATE_ID = 'require_approval';

const KNOWN_APPROVAL_TOOLS: ReadonlySet<string> = new Set(Object.keys(settings.approvalToolInfo));

/** What one row of the Approvals tab's tool list shows. */
export interface ApprovalToolRow {
  /** The tool's wire name. */
  id: string;
  /** The tool's display name, or its id when this build does not know it. */
  name: string;
  /** What the tool does, `null` for a tool this build does not know. */
  description: string | null;
  /** What can happen once the tool runs automatically, `null` when unknown. */
  risk: string | null;
  /** Whether this tool currently runs without waiting for approval. */
  autoApprove: boolean;
  /** "Runs automatically" / "Waits for approval". */
  stateLabel: string;
  /** Whether the risk line should be shown in its emphasized color. */
  isRiskEmphasized: boolean;
}

/** What the Approvals tab's plan-review row shows. */
export interface PlanReviewRow {
  /** Whether a newly created plan currently waits for review before it runs. */
  required: boolean;
  /** "Review first" / "Runs without review". */
  stateLabel: string;
  /** What can happen once a plan is not reviewed, `null` while it is. */
  risk: string | null;
}

/**
 * A known tool's own text, only for one of the ids this build ships display
 * text for; `null` for every other id (`toString`, an id a newer daemon
 * added, ...). Checked against the fixed key set before indexing, so an id
 * that only resembles a native `Object.prototype` member never reads it.
 */
function knownToolInfo(id: string): (typeof settings.approvalToolInfo)[KnownApprovalTool] | null {
  if (!KNOWN_APPROVAL_TOOLS.has(id)) {
    return null;
  }
  return settings.approvalToolInfo[id as KnownApprovalTool];
}

/** Builds one row per tool, in the order the daemon sent them. */
export function approvalToolRows(info: ApprovalSettingsInfo): ApprovalToolRow[] {
  return info.tools.map((tool) => {
    const known = knownToolInfo(tool.tool);
    return {
      id: tool.tool,
      name: known?.name ?? tool.tool,
      description: known?.description ?? null,
      risk: known?.risk ?? null,
      autoApprove: tool.autoApprove,
      stateLabel: tool.autoApprove
        ? settings.approvalRunsAutomatically
        : settings.approvalWaitsForApproval,
      isRiskEmphasized: tool.autoApprove,
    };
  });
}

/** Builds the plan-review row. Its state is its own, never derived from `tools`. */
export function planReviewRow(info: ApprovalSettingsInfo): PlanReviewRow {
  return info.plansRequireApproval
    ? { required: true, stateLabel: settings.planReviewFirst, risk: null }
    : {
        required: false,
        stateLabel: settings.planRunsWithoutReview,
        risk: settings.planReviewOffRisk,
      };
}

/**
 * Whether the connected daemon has settled on a wire revision that carries
 * the approval settings. An unreported revision is not unsupported: the
 * daemon's own refusal is the authority, so this is only ever a negative
 * signal (mirrors `isProfileSettingsReadOnly`'s discipline).
 */
export function isApprovalSettingsSupported(protocolRevision: number | null): boolean {
  return protocolRevision === null || protocolRevision >= APPROVAL_SETTINGS_MIN_REVISION;
}

/**
 * The one line explaining why the Approvals tab cannot be trusted or edited
 * right now, `null` when there is nothing to say. An unsupported daemon is
 * judged before `info`, since an old daemon's settings (if somehow present)
 * are not the live values this tab would otherwise show.
 */
export function approvalSettingsNotice(
  info: ApprovalSettingsInfo | null,
  protocolRevision: number | null
): string | null {
  if (!isApprovalSettingsSupported(protocolRevision)) {
    return settings.approvalSettingsNeedNewerDaemon;
  }
  if (info === null) {
    return null;
  }
  if (info.playbook === 'failed') {
    return settings.approvalPlaybookFailed;
  }
  if (info.playbook === 'not_configured') {
    return settings.approvalPlaybookNotConfigured;
  }
  if (info.playbook === 'unknown') {
    return settings.approvalPlaybookUnknown;
  }
  return null;
}

/** Whether every switch on the Approvals tab must stay frozen. */
export function isApprovalSettingsReadOnly(
  info: ApprovalSettingsInfo | null,
  protocolRevision: number | null
): boolean {
  if (!isApprovalSettingsSupported(protocolRevision)) {
    return true;
  }
  return info === null || info.playbook !== 'loaded';
}

/**
 * The one line a failed switch leaves behind, naming the id, the daemon's
 * reason, and the state the switch stayed in.
 */
export function approvalUpdateFailure(
  id: string,
  reason: string,
  wasOn: boolean,
  kind: 'tool' | 'plan'
): string {
  return kind === 'tool'
    ? settings.approvalToolUpdateFailure(id, reason, wasOn)
    : settings.planUpdateFailure(id, reason, wasOn);
}
