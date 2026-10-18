import { banners, describeThemeIssues } from './ui-messages';
import type { WithdrawReason } from './session/approval-types';
import type {
  ClientConfigFailure,
  DaemonRecoveryStatus,
  StartupEnvFailure,
} from './session/recovery-types';

/** Identifies one of the fixed set of application-wide banners. */
export type BannerId =
  | 'startup-env-failure'
  | 'daemon-spawn'
  | 'daemon-recovery'
  | 'client-config-failure'
  | 'operation-error'
  | 'detection-warning'
  | 'keymap-notice'
  | 'boundary-error'
  | 'themes-load-failure'
  | 'themes-issues'
  | 'approval-expired-unseen'
  | 'approver-registration-refused'
  | 'daemon-protocol-stale'
  | 'profile-unavailable';

/** A one-line summary of the approval request a banner reports. */
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

/**
 * The button a banner offers. A label key, never a handler: the view owns
 * what a click does, so this module stays free of behavior.
 */
export type BannerAction =
  | { labelKey: 'reconnect' }
  | {
      labelKey: 'howToInstall';
      /** The profile whose Settings row shows the install guidance. */
      profileId: string;
    };

/** A profile a banner names. */
export interface NamedProfile {
  id: string;
  displayName: string;
}

/** The least of a profile entry {@link profilesNeedingNativeProgram} reads. */
export interface ProfileAvailability extends NamedProfile {
  enabled: boolean;
  unavailableKind: string | null;
}

/** A banner ready to render: which one, what it says, and what it offers. */
export type BannerEntry = { id: BannerId; text: string; action?: BannerAction };

/**
 * The inputs that decide which banners are active. Kept free of `ipc/`
 * dependencies (`hasThemeLoadFailure` is a plain boolean, not a store type)
 * so this module stays a pure state -> view derivation.
 */
export type BannerState = {
  /**
   * What the launcher reported about how the daemon was started, `null` while
   * nothing was reported. Carries the launcher's own sentence rather than a
   * flag, so the banner names the mode that was used and what follows from it.
   */
  daemonSpawnNotice: string | null;
  operationError: string | null;
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
   * while none have. More than one can pile up between visits to the banner
   * area, so this counts them and keeps the most recent for the summary.
   */
  approvalExpiredUnseen: { count: number; latest: ApprovalExpiredSummary } | null;
  /** Whether this connection's `register_approver` attempt was refused. */
  approverRegistrationRefused: boolean;
  /** How far daemon recovery has got. */
  daemonRecovery: DaemonRecoveryStatus;
  /** The generation `daemonRecovery` was taken at; a smaller one never wins. */
  daemonRecoveryRev: number;
  /** Whether the user has dismissed the daemon-recovery banner. */
  isDaemonRecoveryDismissed: boolean;
  /** Why the client settings could not be loaded, `null` when they were. */
  clientConfigFailure: ClientConfigFailure | null;
  /**
   * Whether the connected daemon is known to have settled on a wire revision
   * older than what showing a window needs.
   * Informational: `false` both while nothing has been reported yet and once
   * a new-enough revision has settled, since the WebView cannot tell those
   * apart from the pushed revision alone.
   */
  isDaemonProtocolStale: boolean;
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
  /** Whether the user has dismissed the banner naming them. */
  isProfileUnavailableDismissed: boolean;
};

/**
 * Derives the active banners from state, in the fixed display order:
 * startup-env-failure -> daemon-spawn -> daemon-recovery ->
 * client-config-failure -> operation-error -> detection-warning ->
 * keymap-notice -> boundary-error -> themes-load-failure -> themes-issues ->
 * approval-expired-unseen -> approver-registration-refused ->
 * daemon-protocol-stale -> profile-unavailable.
 * @param state the current banner-relevant application state
 */
export function buildBanners(state: BannerState): BannerEntry[] {
  const entries: BannerEntry[] = [];

  // First of all: while this holds, the GUI neither spawned nor connected a
  // daemon, so it is the cause behind the absence of everything below.
  if (state.startupEnvFailure !== null) {
    entries.push({
      id: 'startup-env-failure',
      text:
        state.startupEnvFailure === 'invalid_instance_id'
          ? banners.invalidInstanceId
          : banners.storeOverrideWithNamedInstance,
    });
  }
  if (state.daemonSpawnNotice !== null) {
    entries.push({ id: 'daemon-spawn', text: state.daemonSpawnNotice });
  }
  if (state.daemonRecovery !== 'idle' && !state.isDaemonRecoveryDismissed) {
    const isExhausted = state.daemonRecovery === 'exhausted';
    const action: BannerAction = { labelKey: 'reconnect' };
    entries.push({
      id: 'daemon-recovery',
      text: isExhausted ? banners.daemonReconnectFailed : banners.daemonReconnecting,
      ...(isExhausted ? { action } : {}),
    });
  }
  // An absent settings file is the ordinary state of a fresh install, so only
  // a file that exists but could not be used is worth reporting to the user.
  if (state.clientConfigFailure !== null && state.clientConfigFailure !== 'not_found') {
    entries.push({ id: 'client-config-failure', text: banners.clientConfigLoadFailed });
  }
  if (state.operationError !== null) {
    entries.push({ id: 'operation-error', text: state.operationError });
  }
  if (state.detectionWarning !== null) {
    entries.push({ id: 'detection-warning', text: state.detectionWarning });
  }
  if (state.keymapNotice !== null) {
    entries.push({ id: 'keymap-notice', text: state.keymapNotice });
  }
  if (state.boundaryNotice !== null) {
    entries.push({ id: 'boundary-error', text: state.boundaryNotice });
  }
  if (state.hasThemeLoadFailure && !state.isThemeLoadFailureDismissed) {
    entries.push({ id: 'themes-load-failure', text: banners.themesLoadFailed });
  }
  if (state.themeIssueCount > 0 && !state.isThemeIssuesDismissed) {
    entries.push({
      id: 'themes-issues',
      text: describeThemeIssues(state.themeIssueCount),
    });
  }
  if (state.approvalExpiredUnseen !== null) {
    entries.push({
      id: 'approval-expired-unseen',
      text: banners.approvalExpiredUnseen(
        state.approvalExpiredUnseen.count,
        state.approvalExpiredUnseen.latest
      ),
    });
  }
  if (state.approverRegistrationRefused) {
    entries.push({
      id: 'approver-registration-refused',
      text: banners.approverRegistrationRefused,
    });
  }
  if (state.isDaemonProtocolStale) {
    entries.push({ id: 'daemon-protocol-stale', text: banners.daemonProtocolStale });
  }
  const [firstNative] = state.profilesNeedingNativeProgram;
  if (firstNative !== undefined && !state.isProfileUnavailableDismissed) {
    entries.push({
      id: 'profile-unavailable',
      text: banners.profileNeedsNativeProgram(
        state.profilesNeedingNativeProgram.map((profile) => profile.displayName)
      ),
      action: { labelKey: 'howToInstall', profileId: firstNative.id },
    });
  }

  return entries;
}

/**
 * Returns a new state with only the targeted banner dismissed. Leaves
 * `hasThemeLoadFailure` and `themeIssueCount` untouched: those are derived
 * facts, not dismissal flags.
 * @param state the current banner-relevant application state
 * @param id the banner to dismiss
 */
export function dismissBanner(state: BannerState, id: BannerId): BannerState {
  switch (id) {
    case 'startup-env-failure':
      return { ...state, startupEnvFailure: null };
    case 'daemon-spawn':
      return { ...state, daemonSpawnNotice: null };
    case 'daemon-recovery':
      return { ...state, isDaemonRecoveryDismissed: true };
    case 'client-config-failure':
      return { ...state, clientConfigFailure: null };
    case 'operation-error':
      return { ...state, operationError: null };
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
    case 'approver-registration-refused':
      return { ...state, approverRegistrationRefused: false };
    case 'daemon-protocol-stale':
      return { ...state, isDaemonProtocolStale: false };
    case 'profile-unavailable':
      return { ...state, isProfileUnavailableDismissed: true };
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
 * later than the one already held is dropped (it arrived out of order); an
 * adopted change only clears the dismissed flag when the status itself moved,
 * so re-announcing the same status at a later generation leaves a dismissal
 * standing.
 * @param state the current banner-relevant application state
 * @param incoming the status change that arrived, with its generation
 */
export function applyRecoveryUpdate(
  state: BannerState,
  incoming: DaemonRecoveryUpdate
): BannerState {
  if (incoming.rev <= state.daemonRecoveryRev) {
    return state;
  }
  const statusChanged = incoming.status !== state.daemonRecovery;
  return {
    ...state,
    daemonRecovery: incoming.status,
    daemonRecoveryRev: incoming.rev,
    isDaemonRecoveryDismissed: statusChanged ? false : state.isDaemonRecoveryDismissed,
  };
}

/**
 * Records why the client settings could not be loaded, `null` clearing it.
 * Every reason is stored, including the ones `buildBanners` stays silent
 * about, so callers can tell an absent file from an unusable one.
 * @param state the current banner-relevant application state
 * @param failure the reason the settings were not used, `null` when they were
 */
export function applyClientConfigFailure(
  state: BannerState,
  failure: ClientConfigFailure | null
): BannerState {
  return { ...state, clientConfigFailure: failure };
}

/**
 * Counts one approval request that has just been withdrawn, unless the user
 * had already seen it, or the withdrawal was not a timeout: the banner exists
 * to report requests that expired unnoticed, not every way a request can
 * leave the queue (a decision made elsewhere, or the
 * requester disconnecting, is not an "expired unseen" request).
 * @param state the current banner-relevant application state
 * @param wasShown whether the request had been displayed before it was withdrawn
 * @param reason why the request was withdrawn
 * @param summary the withdrawn request, summarized for display
 */
export function recordApprovalExpiredUnseen(
  state: BannerState,
  wasShown: boolean,
  reason: WithdrawReason,
  summary: ApprovalExpiredSummary
): BannerState {
  if (wasShown || reason !== 'timeout') {
    return state;
  }
  const count = (state.approvalExpiredUnseen?.count ?? 0) + 1;
  return { ...state, approvalExpiredUnseen: { count, latest: summary } };
}

/**
 * Clears the unseen-expiry count. Nothing else drops it: the user dismissing
 * the banner is the only evidence that the report was read.
 * @param state the current banner-relevant application state
 */
export function dismissApprovalExpiredUnseen(state: BannerState): BannerState {
  return { ...state, approvalExpiredUnseen: null };
}
