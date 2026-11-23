import { banners, describeReopenNotice, describeThemeIssues } from '../ui-messages';
import type { WithdrawReason } from '../session/approval-types';
import type { DisplayLine } from '../display/untrusted-text';
import type {
  ClientConfigFailure,
  DaemonRecoveryStatus,
  StartupEnvFailure,
} from '../session/recovery-types';
import type { EventKey } from './event-log';

/** Identifies a notice that is derived from application state. */
export type StateNoticeKey =
  | 'startup-env-failure'
  | 'daemon-spawn'
  | 'daemon-recovery'
  | 'daemon-health'
  | 'client-config-failure'
  | 'detection-warning'
  | 'keymap-notice'
  | 'boundary-error'
  | 'themes-load-failure'
  | 'themes-issues'
  | 'approval-expired-unseen'
  | 'approver-registration-refused'
  | 'daemon-incompatible'
  | 'profile-unavailable'
  | 'reopen-offer';

/** Identifies one kind of notice: a state the app is in, or something that happened. */
export type NoticeKey = StateNoticeKey | EventKey;

/** The settings rows a notice kind can be switched off from, in the order the settings table lists them. */
export const NOTICE_CATEGORIES = [
  'needs-approval',
  'needs-input',
  'window-exits',
  'reconnected',
  'failed-actions',
  'setup-problems',
  'last-session',
] as const;

/** One settings row a notice kind can be switched off from. */
export type NoticeCategory = (typeof NOTICE_CATEGORIES)[number];

/** How a kind of notice is treated, wherever it is shown. */
export interface NoticeClass {
  /** Whether the app cannot do its job while this holds, so it is never dismissed or switched off. */
  blocking: boolean;
  tone: 'info' | 'warning' | 'error';
  dismissible: boolean;
  /** The settings row that switches this kind off; `null` when it cannot be switched off. */
  category: NoticeCategory | null;
}

/** What a notice's class can depend on beyond its key. */
export interface NoticeFacts {
  daemonRecovery?: DaemonRecoveryStatus;
  /** The exit code of the window that ended, `null` when it is not known. */
  exitCode?: number | null;
}

/** A one-line summary of the approval request a notice reports. */
export interface ApprovalExpiredSummary {
  /** Who asked, as it would have been shown in the dialog. */
  requester: string;
  /** The operation that was asked for. */
  tool: string;
  /** The window it would have acted on. */
  target: string;
}

/** A status change of daemon recovery, with the generation that orders it. */
export type DaemonRecoveryUpdate = {
  /** The status recovery has moved to. */
  status: DaemonRecoveryStatus;
  /** The generation of that change; it only ever grows. */
  rev: number;
};

/** A change of whether the daemon can be used, with the generation that orders it. */
export type DaemonCompatibilityUpdate = {
  /** Whether the control connection is open to a daemon of another wire revision. */
  incompatible: boolean;
  /** The generation of that change; it only ever grows. */
  rev: number;
};

/**
 * The button a notice offers. A label key, never a handler: the view owns
 * what a click does, so this module stays free of behavior.
 */
export type NoticeAction =
  | { labelKey: 'reconnect' }
  | { labelKey: 'chooseWindows' }
  | {
      labelKey: 'howToInstall';
      /** The profile whose Settings row shows the install guidance. */
      profileId: string;
    };

/** A profile a notice names. */
export interface NamedProfile {
  id: string;
  displayName: DisplayLine;
}

/** The least of a profile entry {@link profilesNeedingNativeProgram} reads. */
export interface ProfileAvailability extends NamedProfile {
  enabled: boolean;
  unavailableKind: string | null;
}

/** A notice ready to render, before anything numbers it. */
export interface NoticeDraft extends Pick<NoticeClass, 'blocking' | 'tone' | 'dismissible'> {
  key: StateNoticeKey;
  /** One line. For a blocking notice the sentence under it is `detail`; otherwise this is the sentence. */
  title: string;
  detail: string | null;
  action: NoticeAction | null;
}

/** The last session's windows on offer to reopen. */
export interface ReopenOffer {
  count: number;
  /** The formatted time it was saved, `null` when unknown. */
  timestamp: string | null;
}

/**
 * The inputs that decide which state notices are active. Kept free of `ipc/`
 * dependencies (`hasThemeLoadFailure` is a plain boolean, not a store type)
 * so this module stays a pure state -> view derivation.
 */
export type NoticeSourceState = {
  /**
   * What the launcher reported about how the daemon was started, `null` while
   * nothing was reported. Carries the launcher's own sentence rather than a
   * flag, so the notice names the mode that was used and what follows from it.
   */
  daemonSpawnNotice: string | null;
  /** Already-formatted text of the last detection-load warning. */
  detectionWarning: string | null;
  keymapNotice: string | null;
  /** Already-formatted text of the last boundary failure, `null` while none is active. */
  boundaryNotice: string | null;
  hasThemeLoadFailure: boolean;
  isThemeLoadFailureDismissed: boolean;
  themeIssueCount: number;
  isThemeIssuesDismissed: boolean;
  /**
   * Approval requests that expired before they were ever displayed, `null`
   * while none have. More than one can pile up between visits to the notice
   * area, so this counts them and keeps the most recent for the summary.
   */
  approvalExpiredUnseen: { count: number; latest: ApprovalExpiredSummary } | null;
  /** Whether this connection's `register_approver` attempt was refused. */
  approverRegistrationRefused: boolean;
  /** How far daemon recovery has got. */
  daemonRecovery: DaemonRecoveryStatus;
  /** The generation `daemonRecovery` was taken at; a smaller one never wins. */
  daemonRecoveryRev: number;
  /** Why the client settings could not be loaded, `null` when they were. */
  clientConfigFailure: ClientConfigFailure | null;
  /**
   * Whether the control connection is open to a daemon that does not speak
   * this build's wire revision. Written only from the core's compatibility
   * changes, never inferred from another fact.
   */
  isDaemonIncompatible: boolean;
  /** The generation `isDaemonIncompatible` was taken at; a smaller one never wins. */
  daemonCompatibilityRev: number;
  /**
   * Why start-up could not resolve which instance this process belongs to,
   * `null` when it could. Carries the reason code
   * only: the identifier that was rejected came from the environment and is
   * never echoed back.
   */
  startupEnvFailure: StartupEnvFailure | null;
  /**
   * The enabled profiles that cannot be used because their program is a
   * Windows `.cmd` / `.bat` file, in list order.
   */
  profilesNeedingNativeProgram: readonly NamedProfile[];
  /** Whether the user has dismissed the notice naming them. */
  isProfileUnavailableDismissed: boolean;
  /** What the daemon says keeps it from opening windows, one sentence each. */
  daemonHealthNotices: readonly string[];
  /** The last session's windows, `null` when there are none to reopen. */
  lastSession: ReopenOffer | null;
  /** Whether the user has dismissed the offer to reopen them. */
  isReopenOfferDismissed: boolean;
  /** Whether any window is open; with none, the empty work area makes the offer. */
  hasOpenWindows: boolean;
};

const BLOCKING_ERROR: NoticeClass = {
  blocking: true,
  tone: 'error',
  dismissible: false,
  category: null,
};
const BLOCKING_WARNING: NoticeClass = { ...BLOCKING_ERROR, tone: 'warning' };
const SETUP_PROBLEM: NoticeClass = {
  blocking: false,
  tone: 'warning',
  dismissible: true,
  category: 'setup-problems',
};
const FAILED_ACTION_WARNING: NoticeClass = {
  blocking: false,
  tone: 'warning',
  dismissible: true,
  category: 'failed-actions',
};

/** One rule per kind, so a new kind cannot be added without saying how it is treated. */
const CLASSIFIERS: Record<NoticeKey, (facts: NoticeFacts) => NoticeClass> = {
  'startup-env-failure': () => BLOCKING_ERROR,
  // Only a retry in progress is mild; anything else means the daemon is not coming back by itself.
  'daemon-recovery': (facts) => ({
    ...BLOCKING_ERROR,
    tone: facts.daemonRecovery === 'retrying' ? 'info' : 'error',
  }),
  'daemon-health': () => BLOCKING_ERROR,
  'approver-registration-refused': () => BLOCKING_WARNING,
  'daemon-incompatible': () => BLOCKING_WARNING,
  'daemon-spawn': () => SETUP_PROBLEM,
  'client-config-failure': () => SETUP_PROBLEM,
  'detection-warning': () => SETUP_PROBLEM,
  'keymap-notice': () => SETUP_PROBLEM,
  'themes-load-failure': () => SETUP_PROBLEM,
  'themes-issues': () => SETUP_PROBLEM,
  'profile-unavailable': () => SETUP_PROBLEM,
  'boundary-error': () => ({ ...FAILED_ACTION_WARNING, tone: 'error' }),
  // An expired permission request is a safety gate: it is not a row the user can switch off.
  'approval-expired-unseen': () => ({ ...FAILED_ACTION_WARNING, category: null }),
  'reopen-offer': () => ({
    blocking: false,
    tone: 'info',
    dismissible: true,
    category: 'last-session',
  }),
  'daemon-reconnected': () => ({
    blocking: false,
    tone: 'info',
    dismissible: true,
    category: 'reconnected',
  }),
  'operation-failure': () => ({ ...FAILED_ACTION_WARNING, tone: 'error' }),
  'window-exit': (facts) => ({
    blocking: false,
    tone: facts.exitCode === 0 ? 'info' : 'warning',
    dismissible: true,
    category: 'window-exits',
  }),
  'restore-load-failed': () => FAILED_ACTION_WARNING,
  'restore-not-restored': () => FAILED_ACTION_WARNING,
};

/**
 * How a kind of notice is treated: whether it blocks, its tone, whether it can
 * be dismissed, and which settings row switches it off.
 * @param key the kind of notice
 * @param facts what the class depends on beyond the key
 */
export function classifyNotice(key: NoticeKey, facts: NoticeFacts): NoticeClass {
  return CLASSIFIERS[key](facts);
}

function draftOf(
  key: StateNoticeKey,
  facts: NoticeFacts,
  text: { title: string; detail: string | null },
  action: NoticeAction | null = null
): NoticeDraft {
  const { blocking, tone, dismissible } = classifyNotice(key, facts);
  return { key, blocking, tone, dismissible, ...text, action };
}

/** A blocking notice says what is wrong in a line and keeps the long sentence beneath it. */
function blockingText(title: string, detail: string): { title: string; detail: string } {
  return { title, detail };
}

function sentenceText(sentence: string): { title: string; detail: null } {
  return { title: sentence, detail: null };
}

/**
 * The offer to reopen the last session's windows, `null` once the user has
 * dismissed it. The notice and the empty work area both read this, so closing
 * one closes both.
 * @param state the current notice-relevant application state
 */
export function reopenOffer(state: NoticeSourceState): ReopenOffer | null {
  return state.isReopenOfferDismissed ? null : state.lastSession;
}

/**
 * Derives the active state notices. The blocking ones come first, in the order
 * of what causes what: the cause behind the absence of the rest leads.
 * @param state the current notice-relevant application state
 */
export function buildNotices(state: NoticeSourceState): NoticeDraft[] {
  const drafts: NoticeDraft[] = [];
  const titles = banners.noticeTitles;

  // While this holds the GUI neither spawned nor connected a daemon, so it is
  // the cause behind the absence of everything below.
  if (state.startupEnvFailure !== null) {
    drafts.push(
      draftOf(
        'startup-env-failure',
        {},
        blockingText(
          titles.startupEnvFailure,
          state.startupEnvFailure === 'invalid_instance_id'
            ? banners.invalidInstanceId
            : banners.storeOverrideWithNamedInstance
        )
      )
    );
  }
  if (state.daemonRecovery !== 'idle') {
    const isExhausted = state.daemonRecovery === 'exhausted';
    drafts.push(
      draftOf(
        'daemon-recovery',
        { daemonRecovery: state.daemonRecovery },
        isExhausted
          ? blockingText(titles.daemonUnreachable, banners.daemonReconnectFailed)
          : blockingText(titles.daemonReconnecting, banners.daemonReconnecting),
        isExhausted ? { labelKey: 'reconnect' } : null
      )
    );
  }
  for (const sentence of state.daemonHealthNotices) {
    drafts.push(draftOf('daemon-health', {}, blockingText(titles.daemonHealth, sentence)));
  }
  if (state.approverRegistrationRefused) {
    drafts.push(
      draftOf(
        'approver-registration-refused',
        {},
        blockingText(titles.approverRegistrationRefused, banners.approverRegistrationRefused)
      )
    );
  }
  if (state.isDaemonIncompatible) {
    drafts.push(
      draftOf(
        'daemon-incompatible',
        {},
        blockingText(titles.daemonIncompatible, banners.daemonIncompatible)
      )
    );
  }

  if (state.daemonSpawnNotice !== null) {
    drafts.push(draftOf('daemon-spawn', {}, sentenceText(state.daemonSpawnNotice)));
  }
  // An absent settings file is the ordinary state of a fresh install, so only
  // a file that exists but could not be used is worth reporting to the user.
  if (state.clientConfigFailure !== null && state.clientConfigFailure !== 'not_found') {
    drafts.push(draftOf('client-config-failure', {}, sentenceText(banners.clientConfigLoadFailed)));
  }
  if (state.detectionWarning !== null) {
    drafts.push(draftOf('detection-warning', {}, sentenceText(state.detectionWarning)));
  }
  if (state.keymapNotice !== null) {
    drafts.push(draftOf('keymap-notice', {}, sentenceText(state.keymapNotice)));
  }
  if (state.boundaryNotice !== null) {
    drafts.push(draftOf('boundary-error', {}, sentenceText(state.boundaryNotice)));
  }
  if (state.hasThemeLoadFailure && !state.isThemeLoadFailureDismissed) {
    drafts.push(draftOf('themes-load-failure', {}, sentenceText(banners.themesLoadFailed)));
  }
  if (state.themeIssueCount > 0 && !state.isThemeIssuesDismissed) {
    drafts.push(
      draftOf('themes-issues', {}, sentenceText(describeThemeIssues(state.themeIssueCount)))
    );
  }
  if (state.approvalExpiredUnseen !== null) {
    drafts.push(
      draftOf(
        'approval-expired-unseen',
        {},
        sentenceText(
          banners.approvalExpiredUnseen(
            state.approvalExpiredUnseen.count,
            state.approvalExpiredUnseen.latest
          )
        )
      )
    );
  }
  const [firstNative] = state.profilesNeedingNativeProgram;
  if (firstNative !== undefined && !state.isProfileUnavailableDismissed) {
    drafts.push(
      draftOf(
        'profile-unavailable',
        {},
        sentenceText(
          banners.profileNeedsNativeProgram(
            state.profilesNeedingNativeProgram.map((profile) => profile.displayName)
          )
        ),
        { labelKey: 'howToInstall', profileId: firstNative.id }
      )
    );
  }
  const offer = reopenOffer(state);
  if (offer !== null && state.hasOpenWindows) {
    drafts.push(
      draftOf(
        'reopen-offer',
        {},
        sentenceText(describeReopenNotice(offer.count, offer.timestamp)),
        { labelKey: 'chooseWindows' }
      )
    );
  }

  return drafts;
}

/**
 * Returns a new state with only the targeted notice dismissed. A blocking
 * notice is not dismissible: it stays until its cause is gone. Leaves
 * `hasThemeLoadFailure` and `themeIssueCount` untouched: those are derived
 * facts, not dismissal flags.
 * @param state the current notice-relevant application state
 * @param key the notice to dismiss
 */
export function dismissNotice(state: NoticeSourceState, key: StateNoticeKey): NoticeSourceState {
  switch (key) {
    case 'daemon-spawn':
      return { ...state, daemonSpawnNotice: null };
    case 'client-config-failure':
      return { ...state, clientConfigFailure: null };
    case 'detection-warning':
      return { ...state, detectionWarning: null };
    case 'keymap-notice':
      return { ...state, keymapNotice: null };
    case 'boundary-error':
      return { ...state, boundaryNotice: null };
    case 'themes-load-failure':
      return { ...state, isThemeLoadFailureDismissed: true };
    case 'themes-issues':
      return { ...state, isThemeIssuesDismissed: true };
    case 'approval-expired-unseen':
      return dismissApprovalExpiredUnseen(state);
    case 'profile-unavailable':
      return { ...state, isProfileUnavailableDismissed: true };
    case 'reopen-offer':
      return { ...state, isReopenOfferDismissed: true };
    case 'startup-env-failure':
    case 'daemon-recovery':
    case 'daemon-health':
    case 'approver-registration-refused':
    case 'daemon-incompatible':
      return state;
  }
}

/**
 * The enabled profiles that cannot be used because their program is a Windows
 * `.cmd` / `.bat` file. A disabled one is left out: the user already chose not
 * to use it.
 * @param profiles the profiles the daemon listed, in list order
 */
export function profilesNeedingNativeProgram(
  profiles: readonly ProfileAvailability[]
): NamedProfile[] {
  return profiles
    .filter((profile) => profile.enabled && profile.unavailableKind === 'needs_native_program')
    .map((profile) => ({ id: profile.id, displayName: profile.displayName }));
}

/**
 * Folds one daemon-recovery status change into the state. A generation no
 * later than the one already held is dropped: it arrived out of order.
 * @param state the current notice-relevant application state
 * @param incoming the status change that arrived, with its generation
 */
export function applyRecoveryUpdate(
  state: NoticeSourceState,
  incoming: DaemonRecoveryUpdate
): NoticeSourceState {
  if (!supersedes(incoming.rev, state.daemonRecoveryRev)) {
    return state;
  }
  return { ...state, daemonRecovery: incoming.status, daemonRecoveryRev: incoming.rev };
}

/**
 * Folds one change of whether the daemon can be used into the state, by the
 * same ordering as {@link applyRecoveryUpdate}: the change event and the read
 * a starting view makes can arrive in either order.
 * @param state the current notice-relevant application state
 * @param incoming the change that arrived, with its generation
 */
export function applyCompatibilityUpdate(
  state: NoticeSourceState,
  incoming: DaemonCompatibilityUpdate
): NoticeSourceState {
  if (!supersedes(incoming.rev, state.daemonCompatibilityRev)) {
    return state;
  }
  return {
    ...state,
    isDaemonIncompatible: incoming.incompatible,
    daemonCompatibilityRev: incoming.rev,
  };
}

/** Whether a generation is later than the one held; an equal one arrived twice. */
function supersedes(incoming: number, held: number): boolean {
  return incoming > held;
}

/**
 * Records why the client settings could not be loaded, `null` clearing it.
 * Every reason is stored, including the ones `buildNotices` stays silent
 * about, so callers can tell an absent file from an unusable one.
 * @param state the current notice-relevant application state
 * @param failure the reason the settings were not used, `null` when they were
 */
export function applyClientConfigFailure(
  state: NoticeSourceState,
  failure: ClientConfigFailure | null
): NoticeSourceState {
  return { ...state, clientConfigFailure: failure };
}

/**
 * Counts one approval request that has just been withdrawn, unless the user
 * had already seen it, or the withdrawal was not a timeout: the notice exists
 * to report requests that expired unnoticed, not every way a request can
 * leave the queue (a decision made elsewhere, or the
 * requester disconnecting, is not an "expired unseen" request).
 * @param state the current notice-relevant application state
 * @param wasShown whether the request had been displayed before it was withdrawn
 * @param reason why the request was withdrawn
 * @param summary the withdrawn request, summarized for display
 */
export function recordApprovalExpiredUnseen(
  state: NoticeSourceState,
  wasShown: boolean,
  reason: WithdrawReason,
  summary: ApprovalExpiredSummary
): NoticeSourceState {
  if (wasShown || reason !== 'timeout') {
    return state;
  }
  const count = (state.approvalExpiredUnseen?.count ?? 0) + 1;
  return { ...state, approvalExpiredUnseen: { count, latest: summary } };
}

/**
 * Clears the unseen-expiry count. Nothing else drops it: the user dismissing
 * the notice is the only evidence that the report was read.
 * @param state the current notice-relevant application state
 */
export function dismissApprovalExpiredUnseen(state: NoticeSourceState): NoticeSourceState {
  return { ...state, approvalExpiredUnseen: null };
}
