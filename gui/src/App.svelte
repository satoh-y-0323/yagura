<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import { get, writable } from 'svelte/store';

  import {
    applyClientConfigFailure,
    applyRecoveryUpdate,
    buildBanners,
    dismissBanner,
    profilesNeedingNativeProgram,
    recordApprovalExpiredUnseen,
    type ApprovalExpiredSummary,
    type BannerAction,
    type BannerId,
  } from './banner-model';
  import { describeBoundaryNotice, type BoundaryOperation } from './boundary-notice-model';
  import type { OpenWindowSubmission, ProfileOption } from './components/add-window-dialog-types';
  import AddWindowDialog from './components/AddWindowDialog.svelte';
  import AppRail from './components/AppRail.svelte';
  import ApprovalDialog from './components/ApprovalDialog.svelte';
  import ChatPanel from './components/ChatPanel.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';
  import FloatWindow from './components/FloatWindow.svelte';
  import IconButton from './components/IconButton.svelte';
  import PlansPanel from './components/PlansPanel.svelte';
  import RenameDialog from './components/RenameDialog.svelte';
  import RestoreBanner from './components/RestoreBanner.svelte';
  import RestoreDialog from './components/RestoreDialog.svelte';
  import SettingsPanel from './components/SettingsPanel.svelte';
  import StatusPanel from './components/StatusPanel.svelte';
  import TabBar from './components/TabBar.svelte';
  import TileIcon from './components/TileIcon.svelte';
  import { bodyEscapePanel } from './components/panel-focus';
  import { pointerDrag, type PointerDragOptions } from './components/pointer-drag';
  import {
    anyNeedsUser,
    applyChatChangedToRooms,
    applyRoomsList,
    chatReloadAfterSeq,
    emptyChatRoom,
    mergeChatMessages,
    type ChatRoomState,
  } from './chat/chat-store';
  import { serializeRefresh } from './chat/refresh-serializer';
  import { daemonProblemNotices } from './daemon-health-notice';
  import type { DropSignal } from './ipc/folder-drop-model';
  import { writeClipboardText } from './ipc/clipboard';
  import { pickFolder, subscribeFolderDrop } from './ipc/folder-picker';
  import { startFramePull, type FramePull } from './ipc/frame-pull';
  import {
    sanitizeForDisplay,
    ATTACH_VIEW_MIN_REVISION,
    type ApprovalRequest,
    type ChatChanged,
    type ChatRoomInfo,
    type RestoreEntry,
  } from './ipc/session-boundary';
  import {
    invokeNextFrames,
    subscribeChatChanged,
    subscribeChatParticipantsChanged,
    subscribeSession,
    subscribeWindowList,
    invokeBroadcastChat,
    invokeGetApprovalSettings,
    invokeListChatRooms,
    invokeListRestoreCandidates,
    invokeOpenWindow,
    invokeCloseWindow,
    invokeDecideApproval,
    invokeGetRecoveryStatus,
    invokeGetDaemonHealth,
    invokeSyncApprovals,
    invokeInterruptWindow,
    invokeListDetections,
    invokeListProfiles,
    invokeListWindows,
    invokeKeymapFilePath,
    invokeLoadKeymap,
    invokeSaveKeymap,
    invokeLoadThemes,
    invokeReadChat,
    invokeReconnectDaemon,
    invokeRenameWindow,
    invokeSendChat,
    invokeSetAutoApprove,
    invokeSetPlanApprovalRequired,
    invokeSetProfileEnabled,
    invokeUpdateWindowLayout,
    type ApprovalSettingsInfo,
    type OpenWindowResult,
    type WindowInfo,
  } from './ipc/session-client';
  import type {
    PlanChanged,
    PlanInfo,
    PlanSummary,
    ReviewMark,
    RoleEntry,
    TaskAssignment,
  } from './ipc/plan-boundary';
  import {
    invokeApprovePlan,
    invokeClearTaskAssignment,
    invokeDeletePlan,
    invokeGetPlan,
    invokeListPlans,
    invokeListRoles,
    invokeMarkPlanSeen,
    invokeSaveRoles,
    invokeSetPlanOwner,
    invokeSetTaskAssignment,
    subscribePlanChanged,
  } from './ipc/plan-client';
  import {
    MODEL_ALIASES_MIN_REVISION,
    type LaunchOptionAlias,
    type ModelChoicesPayload,
    type ShippedModelProfiles,
  } from './ipc/model-boundary';
  import {
    invokeListModelChoices,
    invokeSaveModelAliases,
    listenModelChoices,
  } from './ipc/model-client';
  import { PROFILE_DETAIL_MIN_REVISION, type ProfileDetail } from './ipc/profile-detail-boundary';
  import { invokeGetProfileDetail, invokeSetProfileCommand } from './ipc/profile-detail-client';
  import { shippedAliasesOf } from './profile/model-aliases-model';
  import {
    platformName,
    type ProfileDetailActions,
    type ProfileDetailsState,
  } from './settings/profile-detail-model';
  import type {
    McpRegistrationAction,
    McpRegistrationResultPayload,
    McpRegistrationsListed,
    McpRegistrationStatusInfo,
    McpRegistrationStatusPayload,
  } from './ipc/mcp-registration-boundary';
  import {
    invokeListMcpRegistrations,
    invokeRunMcpRegistration,
    listenMcpRegistrationResult,
    listenMcpRegistrationStatus,
  } from './ipc/mcp-registration-client';
  import { hostKeyboardPlatform } from './keymap/keyboard-platform';
  import { createKeymapStore } from './keymap/keymap-store';
  import type { KeymapLoadStatus } from './settings/keyboard-settings-model';
  import { createShortcutDispatcher } from './keymap/shortcut-dispatcher';
  import { createShortcutRunner } from './keymap/shortcut-runner';
  import { cascadeRect, type Size } from './layout/float-layout';
  import { clampSidebarWidth, createSidebarWidthStorage } from './layout/sidebar-width';
  import { nextActiveTab } from './layout/tab-order';
  import { offeredProfiles } from './profile/offered-profiles';
  import {
    createBrowserProfileMemoryDeps,
    createProfileMemory,
    withRememberedOption,
  } from './profile/profile-store';
  import {
    acceptsChoicesPush,
    CHOICES_FETCH_TIMEOUT_MS,
    isChoicesFetchOverdue,
    nextChoicesRequestId,
    type ModelChoicesEntry,
  } from './profile/model-aliases-model';
  import * as approvalQueue from './session/approval-queue';
  import { describeUntargeted } from './session/approval-target';
  import type { WithdrawReason } from './session/approval-types';
  import {
    plansAttention,
    planReloadFor,
    selectionAfterPlanReadFailure,
    reviewMarksOf,
    shouldMarkPlanSeen,
    withRetainedMarks,
    type PlanReload,
  } from './plans/plan-board-model';
  import {
    approvalSettingsNotice,
    approvalUpdateFailure,
    isApprovalSettingsReadOnly,
    isApprovalSettingsSupported,
    PLAN_REVIEW_GATE_ID,
  } from './settings/approval-settings-model';
  import {
    integrationsNotice,
    isIntegrationsSupported,
  } from './settings/integration-settings-model';
  import {
    clampPanelSize,
    panelMaxSize,
    createPanelSizeStorage,
    isResizedByUser,
    type PanelSize as SettingsPanelSize,
  } from './settings/panel-size';
  import { isProfileSettingsReadOnly } from './settings/profile-settings-model';
  import { type SettingsTab } from './settings/settings-tabs';
  import { startLayoutSync } from './session/layout-sync';
  import type {
    ClientConfigFailure,
    DaemonRecoveryStatus,
    StartupEnvFailure,
  } from './session/recovery-types';
  import { confirmCopy, type ConfirmOperation } from './session/confirm-target';
  import { pairPanes, type Pane } from './session/pane-plan';
  import { createAttentionNavigation } from './session/attention-navigation';
  import { currentWindow, isWindowVisible } from './session/current-window';
  import { createRoomActions } from './session/room-actions';
  import {
    createPendingFocus,
    panelReturnTarget,
    restoreFocusMove,
    sidebarToggleMove,
    type FocusMove,
    type PanelOrigin,
  } from './session/focus-moves';
  import {
    applyLayoutSteps,
    applySelectionOverrides,
    defaultSelection,
    formatRestoreTimestamp,
    planLayoutApplication,
    layoutTargetsForOpened,
    layoutStepsForSnapshot,
    isProfileDisabledRefusal,
    restoreFallbackOf,
    restoreLaunchOf,
    restoreNoticeForDisabled,
    type LayoutApplicationTarget,
    type LayoutTarget,
    type RestoreLaunch,
  } from './session/restore-model';
  import { SessionController } from './session/session-controller';
  import { createCollapsedRooms } from './sidebar/collapsed-rooms';
  import {
    groupByRoom,
    roomIdsOf,
    windowIdsOfRoom,
    windowsInDisplayOrder,
    type RoomGroup,
  } from './sidebar/room-groups';
  import {
    createWindowActions,
    type ConfirmPort,
    type WindowActions,
  } from './session/window-actions';
  import { syncWindows } from './session/window-sync';
  import { attentionCount } from './status-tone';
  import { createWindowSessionStore } from './stores/session-store';
  import { createWindowRegistry, type WindowEntry } from './stores/window-registry';
  import { bindPalette } from './theme/theme-apply';
  import { isBuiltInSelection, type AppThemeSelection } from './theme/theme-resolve';
  import { createBrowserThemeDeps, createThemeStore } from './theme/theme-store';
  import {
    buttons,
    chat,
    dialogs,
    forms,
    errors,
    banners,
    hints,
    nav,
    themes,
    describeDetectionWarning,
    describeDiscardedKeymapEntries,
    describeOperationFailure,
    describeThemeName,
    foldExternalText,
    settings,
  } from './ui-messages';
  import { shouldDropRememberedCwd, windowOpenFailureOf } from './window-open-failure-model';

  // Add-window dialog state. The detection definition the dialog shows is
  // held here because the option list is derived from it (below). The working
  // directory is held here too: `Browse…` and folder drops reach Tauri, and
  // that side of the boundary is the shell's, not the dialog's.
  let isAddDialogOpen = false;
  let detectionInput = '';
  let cwdInput = '';
  // The launch profile the dialog shows; `''` is the Custom form.
  let profileInput = '';
  // The `model` / `effort` launch options the dialog shows, held here for
  // the same reason `cwdInput` is: the dialog reports changes back rather
  // than owning the value itself.
  let modelInput = '';
  let effortInput = '';

  // Folder drag-drop. The subscription only exists while the dialog is open;
  // the pending flag keeps a second one from being opened while the
  // first is still coming up.
  let isDropTarget = false;
  let unlistenFolderDrop: (() => void) | null = null;
  let isFolderDropPending = false;

  // Detection definition names the daemon offers, as of the last successful
  // enumeration. A failed enumeration leaves the previous list in place,
  // so the dropdown never empties out because one look failed.
  let detectionNames: string[] = [];

  // Why the last opened window could not load its detection definition. It
  // arrives once the window is open and the dialog has closed, so it is shown
  // on the banner path.
  let detectionWarning: string | null = null;

  // Why the detection definitions could not be enumerated. This one arises
  // while the dialog is open, so the dialog displays it.
  let detectionListWarning: string | null = null;

  // The launch profiles the daemon offers, as of the last successful
  // enumeration, and why the last one failed. Their own pair of variables:
  // a profile-list failure must not overwrite the detection-list warning,
  // which is about a different list and a different field.
  let profileOptions: ProfileOption[] = [];
  let profileListWarning: string | null = null;

  // What was launched from the dialog last time at this terminal, which is
  // what it opens on.
  const profileMemory = createProfileMemory(createBrowserProfileMemoryDeps());

  // Rename dialog state
  let renameDialog: { isOpen: boolean; entry: WindowEntry | null } = {
    isOpen: false,
    entry: null,
  };

  // Restore flow. `restoreLoadFailedNotice` is not part of
  // banner-model.ts's fixed banner set (RestoreBanner is deliberately not
  // integrated there): it renders through its own element
  // below.
  let restoreCandidates: RestoreEntry[] = [];
  let restoreTakenAtEpochS: number | null = null;
  let isRestoreBannerDismissed = false;
  // Dismissing the banner naming unusable profiles holds for this run only.
  let isProfileUnavailableDismissed = false;
  let isRestoreDialogOpen = false;
  let restoreSelection: Set<number> = new Set();
  // Checkboxes the user toggled by hand (position -> checked); never rendered
  // directly, so there is no clearing path -- it lives for the process lifetime.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity -- lookup table read only when the dialog opens, never rendered, so runes-based SvelteMap reactivity does not apply
  const restoreSelectionOverrides = new Map<number, boolean>();
  let restoreLoadFailedNotice: string | null = null;

  // What the daemon says keeps it from opening windows, one sentence each.
  // Like `restoreLoadFailedNotice` it renders through its own element.
  let daemonHealthNotices: string[] = [];

  // What the launcher reported about how the daemon was started, `null`
  // while nothing was reported. Holds the launcher's own sentence: it is the
  // only place the detachment mode used, and what follows from it, is stated.
  let daemonSpawnNotice: string | null = null;

  // Daemon recovery: how far the supervisor has got, the generation
  // that ordering is decided by, whether the user dismissed the banner, and
  // why the client settings were not used.
  let daemonRecovery: DaemonRecoveryStatus = 'idle';
  let daemonRecoveryRev = 0;
  let isDaemonRecoveryDismissed = false;
  let clientConfigFailure: ClientConfigFailure | null = null;

  // Why start-up could not resolve which instance this process
  // belongs to. While non-null no daemon was started or connected.
  let startupEnvFailure: StartupEnvFailure | null = null;

  // The last boundary failure reported anywhere in the application, `null`
  // while none is active. Carries the daemon's own diagnosis rather than a
  // fixed line, so the banner never claims a cause it did not observe.
  let boundaryNotice: string | null = null;

  // Last window operation that the core or the daemon refused
  let operationError: string | null = null;

  // What the startup keymap load has to report, `null` when it has nothing to
  let keymapNotice: string | null = null;
  const keyboardPlatform = hostKeyboardPlatform();
  const keymapStore = createKeymapStore({
    platform: keyboardPlatform,
    save: invokeSaveKeymap,
  });
  // How the keymap load ended; the Keyboard tab refuses to edit a file that was not read.
  let keymapLoadStatus: KeymapLoadStatus = { kind: 'not-loaded' };
  // The keymap.toml path this instance reads, `null` until known or when it has none.
  let keymapFilePath: string | null = null;

  // Whether the user has dismissed the two theme banners
  let isThemeLoadFailureDismissed = false;
  let isThemeIssuesDismissed = false;

  // Measured size of the float area; every layout calculation is fed from it
  let area: Size = { width: 0, height: 0 };

  // The element tiling measures, so the grid covers the area the windows get
  // once the tab strip is gone; `area` still tracks the float area.
  let mainContentElement: HTMLElement | undefined;

  // How this machine shows the application, not part of the restored session,
  // so the range, the rounding and the persistence all live in ./layout/sidebar-width.
  const sidebarWidthStorage = createSidebarWidthStorage();
  let sidebarWidth = sidebarWidthStorage.load();
  // The drag carries the width it started from.
  const sidebarDrag: PointerDragOptions<number> = {
    start: () => sidebarWidth,
    move: (startWidth, { dx }) => {
      sidebarWidth = clampSidebarWidth(startWidth + dx);
    },
    end: () => {
      sidebarWidthStorage.save(sidebarWidth);
    },
  };

  // The Settings and the Plans panel's sizes similarly live here, not in
  // the restored session: they are how this machine shows the panels, kept
  // in ./settings/panel-size (one storage per panel) alongside the
  // sidebar's own storage. What is kept is the size the user chose; the size
  // shown is derived from it for the current window, so a smaller window
  // never overwrites it.
  const settingsPanelSizeStorage = createPanelSizeStorage('settings');
  let chosenSettingsPanelSize: SettingsPanelSize = settingsPanelSizeStorage.load();
  const plansPanelSizeStorage = createPanelSizeStorage('plans');
  let chosenPlansPanelSize: SettingsPanelSize = plansPanelSizeStorage.load();
  let panelViewport: SettingsPanelSize = currentViewport();
  $: settingsPanelSize = clampPanelSize(chosenSettingsPanelSize, panelViewport);
  $: plansPanelSize = clampPanelSize(chosenPlansPanelSize, panelViewport);
  $: panelMax = panelMaxSize(panelViewport);

  // Keep a reference to unsubscribe from session events
  let unsubscribeSession: (() => void) | null = null;
  let framePull: FramePull | null = null;
  let unsubscribeWindowList: (() => void) | null = null;

  // Confirmation dialog state
  let confirmDialog = { isOpen: false, operation: 'close' as ConfirmOperation };
  let pendingConfirm: ((approved: boolean) => void) | null = null;

  // The chat rooms. `chatRooms` is the room list (`list_chat_rooms`,
  // re-read whenever the panel is open); `chatRoomStates` holds what each
  // room's page reads have merged in, keyed by room id, so switching rooms
  // does not discard what an earlier one held. `currentRoomId` is which room
  // the right-hand column shows, `''` until the user picks one.
  // `chatNeedsUser` is the aggregate badge, held apart from both maps
  // because it has to light up from a push while the panel is closed, which
  // is the one case where no page is read.
  type PanelId = 'chat' | 'plans' | 'settings';
  const PANEL_IDS: readonly PanelId[] = ['chat', 'plans', 'settings'];
  const PANEL_SELECTORS: Readonly<Record<PanelId, string>> = {
    chat: '.chat-panel',
    plans: '.plans-panel',
    settings: '.settings-panel',
  };
  // The panel that last held focus, dropped once focus moves elsewhere: the
  // Escape pressed after focus fell to the body belongs to it.
  let lastFocusedPanel: PanelId | null = null;
  // The open panels, most recently focused first. Focus leaving the panels
  // keeps the order, so closing the upper of two stacked panels finds the lower.
  let recentPanels: PanelId[] = [];
  // Where focus was when each open panel opened.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity -- read only when a panel closes, never rendered, so runes-based SvelteMap reactivity does not apply
  const panelOrigins = new Map<PanelId, PanelOrigin<PanelId, HTMLElement>>();

  let isChatOpen = false;
  let currentRoomId = '';
  let chatRooms: Map<string, ChatRoomInfo> = new Map();
  let chatRoomStates: Map<string, ChatRoomState> = new Map();
  let chatNeedsUser = false;
  let unsubscribeChatChanged: (() => void) | null = null;
  let unsubscribeChatParticipantsChanged: (() => void) | null = null;

  // The plan ledger, open independently of the chat panel. Liveness and the
  // owner candidates are derived by the panel from the window registry, so
  // nothing here is read again when only the window list moves.
  let isPlansOpen = false;
  // Marks the user was shown this time the panel is open; the daemon clears
  // them as soon as the plan is seen, so they are kept here until it closes.
  let retainedMarks: Readonly<Record<string, ReviewMark>> = {};
  $: if (!isPlansOpen) {
    retainedMarks = {};
    // A request to show a profile lives as long as the panel it was made
    // for: the panel's own record of having shown it goes when it closes.
    requestedModelProfile = null;
    modelChoices = new Map();
    latestChoicesRequestIds.clear();
  }
  // What the board shows: the daemon's plan with the retained marks laid over
  // it, so no writer of `plan` has to remember them.
  $: displayedPlan = plan === null ? null : withRetainedMarks(plan, retainedMarks);
  let planList: PlanSummary[] = [];
  let selectedPlanId: string | null = null;
  let plan: PlanInfo | null = null;
  let roles: RoleEntry[] = [];
  // Kept as read so a save does not clear the table's default role.
  let defaultRole: string | null = null;
  let unsubscribePlanChanged: (() => void) | null = null;

  // The Settings panel: its own open flag and active tab, plus the state of
  // the one operation it can start (switching a profile on or off).
  let isSettingsOpen = false;
  let settingsTab: SettingsTab = 'profiles';
  // The profile currently being switched, `null` while none is in flight;
  // handed to the Settings panel so it can freeze the row being saved.
  let savingProfileId: string | null = null;
  // Why the last switch was refused, `null` while none was, or once a later
  // switch or closing the panel supersedes it.
  let profileUpdateFailure: string | null = null;
  // The profile whose details are open in the Profiles tab, what was read
  // for it, and the state of a command save or reset for it.
  let openProfileDetailId: string | null = null;
  let profileDetail: ProfileDetail | null = null;
  let profileDetailFailure: string | null = null;
  // A command save belongs to the profile it was sent for, so another
  // profile's details opened meanwhile never show its state.
  let savingCommandProfileId: string | null = null;
  let profileCommandFailure: { id: string; reason: string } | null = null;
  // A request to show one profile in the Plans panel's Models tab.
  let requestedModelProfile: { id: string; seq: number } | null = null;

  // The Approvals tab: the daemon's current settings, read again each time
  // the panel opens; the one operation it can start (an auto-approve or
  // plan-review switch) shares a save flag and a failure line with every
  // switch on the tab, mirroring the Profiles tab above.
  let approvalSettings: ApprovalSettingsInfo | null = null;
  let isApprovalSaving = false;
  let approvalSwitchFailure: string | null = null;

  // The Models tab. The model lists are read once per profile while the panel
  // is open and forgotten when it closes; each request's id is recorded as the
  // profile's latest before the request is sent, so a push that overtakes the
  // acknowledgement is still taken and an older one is not.
  let shippedModels: ShippedModelProfiles = {};
  let modelChoices: ReadonlyMap<string, ModelChoicesEntry> = new Map();
  // eslint-disable-next-line svelte/prefer-svelte-reactivity -- bookkeeping read only when a push arrives, never rendered, so runes-based SvelteMap reactivity does not apply
  const latestChoicesRequestIds = new Map<string, number>();
  let lastChoicesRequestId = 0;
  // How soon an overdue check that found its timer early looks again.
  const OVERDUE_RECHECK_MS = 250;
  // Whether the connected daemon's last-pushed wire revision predates the
  // model-alias requests. Only decides whether the tab offers editing; a
  // request sent anyway is refused by the core and shown as a failure.
  let isModelAliasesReadOnly = false;
  let unsubscribeModelChoices: (() => void) | null = null;

  // The Integrations tab: the listed rows and each row's status, read again
  // whenever the tab is shown and never polled. Each read's id is recorded
  // before it is sent, so a status push that overtakes the reply is still
  // taken and one from an earlier read is not. At most one operation runs at
  // a time; its result stays under its row until the panel closes.
  let integrationsListed: McpRegistrationsListed | null = null;
  let integrationStatuses: ReadonlyMap<string, McpRegistrationStatusInfo> = new Map();
  let integrationRunning: string | null = null;
  let integrationResults: ReadonlyMap<string, McpRegistrationResultPayload> = new Map();
  let integrationsFailure: string | null = null;
  let integrationsListRequestId: number | null = null;
  let integrationsRunRequestId: number | null = null;
  let lastIntegrationsRequestId = 0;
  let unsubscribeMcpRegistrationStatus: (() => void) | null = null;
  let unsubscribeMcpRegistrationResult: (() => void) | null = null;

  // The connected daemon's last-pushed wire revision, `null` until the first
  // `subscribe_session` push arrives. Kept under the wire field's own name:
  // it is what `isDaemonProtocolStale`, `isModelAliasesReadOnly` and the
  // Settings panel's read-only gate are all derived from.
  let protocol_revision: number | null = null;
  $: isSettingsReadOnly = isProfileSettingsReadOnly(protocol_revision);
  $: profileDetails = {
    // An unreported revision still offers details: the daemon's own refusal
    // is the authority, so they are only withheld on evidence.
    isAvailable: protocol_revision === null || protocol_revision >= PROFILE_DETAIL_MIN_REVISION,
    openId: openProfileDetailId,
    detail: profileDetail,
    failure: profileDetailFailure,
    isSavingCommand: openProfileDetailId !== null && savingCommandProfileId === openProfileDetailId,
    commandFailure:
      profileCommandFailure !== null && profileCommandFailure.id === openProfileDetailId
        ? profileCommandFailure.reason
        : null,
    platformName: platformName(keyboardPlatform),
  } satisfies ProfileDetailsState;
  $: isApprovalReadOnly = isApprovalSettingsReadOnly(approvalSettings, protocol_revision);
  $: approvalNotice = approvalSettingsNotice(approvalSettings, protocol_revision);
  $: integrationsNoticeLine = integrationsNotice(integrationsListed, protocol_revision);

  // Approval queue: the requests the daemon asks this GUI to decide.
  // `approvalDisplayedId` is what the dialog is showing right now, which is
  // how a fresh opening -- and with it a fresh click guard -- is recognized.
  let approvals = approvalQueue.emptyQueue();
  let approvalOpenedAt = 0;
  let approvalDisplayedId: string | null = null;
  let approvalExpiredUnseen: { count: number; latest: ApprovalExpiredSummary } | null = null;
  // Whether this connection's register_approver attempt was refused.
  // Set by the refusal event and by a refused pull, cleared
  // by the reset event and by a registered pull, so a refusal
  // that predates the subscription is still reported and still clears.
  let approverRegistrationRefused = false;
  // Whether the connected daemon's last-pushed wire revision is older than
  // what showing a window needs. Information: the
  // gate that actually decides what may be sent is Rust-side and stays
  // correct even if this field lagged.
  let isDaemonProtocolStale = false;

  const registry = createWindowRegistry();

  // Sidebar room bundling: the collapsed set lives in this process
  // only, and the per-room operations share the same tile
  // measurement as the header's tile-all button (measureTileArea, below).
  const collapsedRooms = createCollapsedRooms();
  const roomActions = createRoomActions({
    registry,
    collapsed: { toggle: collapsedRooms.toggle },
    windowIdsOf: (roomId) => windowIdsOfRoom(get(registry).windows, roomId),
    measureArea: measureTileArea,
  });

  // Reports live layout changes back to the daemon: one
  // subscription for the whole application, mirroring `layout`
  // just below in wiring straight to the registry.
  const stopLayoutSync = startLayoutSync(registry, (windowId, layoutJson) => {
    void invokeUpdateWindowLayout(windowId, layoutJson).catch((error) => {
      console.error('failed to report window layout:', error);
    });
  });

  const themeStore = createThemeStore(createBrowserThemeDeps());

  // The application palette is written to the document root, so every rule
  // that reads a token gets it, and a float window can shadow it on its own
  // subtree.
  const unbindAppPalette = bindPalette(
    themeStore,
    (state) => state.appPalette,
    document.documentElement
  );

  // A store, not a plain map: a pane is rendered as soon as it is created, so
  // creating one has to reach the view.
  const panes = writable<Map<string, Pane>>(new Map());

  // Move and resize are written straight to the registry by `FloatWindow`;
  // this sink narrows what it can reach to those two mutators.
  const layout = {
    setWindowPosition: registry.setWindowPosition,
    setWindowSize: registry.setWindowSize,
  };

  $: themeLoadFailure = $themeStore.loadFailure;
  $: themeIssueCount = $themeStore.issues.length;
  // What every chooser but Settings offers: the profiles the user left
  // switched on. Settings needs the full list -- it is where a disabled one
  // is switched back on -- so it alone is handed the unfiltered profileOptions.
  $: offeredProfileOptions = offeredProfiles(profileOptions);
  // A definition the user picked before the list dropped it stays selectable,
  // so a stale-looking selection is never silently swapped for the default.
  $: detectionOptions =
    detectionInput !== '' && !detectionNames.includes(detectionInput)
      ? [detectionInput, ...detectionNames]
      : detectionNames;
  $: bannerState = {
    daemonSpawnNotice,
    operationError,
    detectionWarning,
    keymapNotice,
    boundaryNotice,
    hasThemeLoadFailure: themeLoadFailure !== null,
    isThemeLoadFailureDismissed,
    themeIssueCount,
    isThemeIssuesDismissed,
    approvalExpiredUnseen,
    approverRegistrationRefused,
    daemonRecovery,
    daemonRecoveryRev,
    isDaemonRecoveryDismissed,
    clientConfigFailure,
    isDaemonProtocolStale,
    startupEnvFailure,
    profilesNeedingNativeProgram: profilesNeedingNativeProgram(profileOptions),
    isProfileUnavailableDismissed,
  };
  $: activeBanners = buildBanners(bannerState);
  // Recomputed on every registry change so the already-open hint (cwd + name
  // match) tracks which windows are actually open right now.
  $: restoreOpenWindows = $registry.windows.map((entry) => ({ cwd: entry.cwd, name: entry.name }));
  $: appThemeValue = fromAppThemeSelection($themeStore.selection);
  $: appThemeOptions = [
    { value: 'dark', label: describeThemeName('dark') },
    { value: 'light', label: describeThemeName('light') },
    { value: 'system', label: themes.systemOption },
    ...Object.keys($themeStore.userPalettes).map((name) => ({
      value: name,
      label: describeThemeName(name),
    })),
  ];

  $: currentRoomName = chatRooms.get(currentRoomId)?.name ?? currentRoomId;
  $: confirmWording =
    confirmDialog.operation === 'broadcast'
      ? {
          ...confirmCopy(confirmDialog.operation),
          message: chat.broadcastConfirmRoomMessage(currentRoomName),
        }
      : confirmCopy(confirmDialog.operation);

  $: approvalRequest = approvalQueue.head(approvals);
  // An approval never stacks on top of another modal: it waits in the queue
  // until whatever the user already has open is closed.
  $: isApprovalDialogOpen =
    approvalRequest !== null &&
    !isAddDialogOpen &&
    !confirmDialog.isOpen &&
    !renameDialog.isOpen &&
    !isRestoreDialogOpen;
  $: isApprovalRequesterOpen = isRequesterWindowOpen(approvalRequest, $registry.windows);
  // Delegated to a function for the same reason as the folder-drop sync
  // above: the statement would otherwise read what it assigns.
  $: void syncApprovalDisplay(isApprovalDialogOpen, approvalRequest);

  // Tab mode: derived, not stored — "in tab mode" is exactly "some
  // window is maximized" (the single-maximized-window invariant the registry
  // already enforces), so there is no separate flag that could drift from it.
  $: tabEntries = $registry.tabOrder
    .map((windowId) => $registry.windows.find((entry) => entry.windowId === windowId))
    .filter((entry): entry is WindowEntry => entry !== undefined);
  $: activeTabEntry = $registry.windows.find((entry) => entry.display.isMaximized);
  // The window keyboard operations act on; derived so no second record of it can drift.
  $: current = currentWindow($registry.windows);

  // The single source the float-area render loop pairs from: an
  // entry only renders once its pane exists, so no flush can read a pane off
  // an entry that has none.
  $: renderedPanes = pairPanes($registry.windows, $panes);

  // Delegated to a function rather than written inline: a reactive statement
  // that assigns the subscription handle it also reads is an infinite
  // reactive loop as far as Svelte is concerned.
  $: void syncFolderDropSubscription(isAddDialogOpen);

  async function syncFolderDropSubscription(isDialogOpen: boolean): Promise<void> {
    if (!isDialogOpen) {
      isDropTarget = false;
      if (unlistenFolderDrop !== null) {
        unlistenFolderDrop();
        unlistenFolderDrop = null;
      }
      return;
    }
    if (unlistenFolderDrop !== null || isFolderDropPending) {
      return;
    }
    isFolderDropPending = true;
    try {
      const unlisten = await subscribeFolderDrop(handleFolderDrop);
      if (isAddDialogOpen) {
        unlistenFolderDrop = unlisten;
      } else {
        unlisten();
      }
    } catch (error) {
      // The webview refused the subscription. Dropping a folder does nothing
      // then, and `Browse…` is still the way to pick one.
      console.error('failed to subscribe to folder drop:', error);
    } finally {
      isFolderDropPending = false;
    }
  }

  function handleFolderDrop(signal: DropSignal): void {
    isDropTarget = signal?.kind === 'over';
    if (signal?.kind === 'drop') {
      cwdInput = signal.path;
    }
  }

  /**
   * The one confirmation port of the application: it opens the
   * dialog and answers the operation with what the user chose.
   */
  const confirmPort: ConfirmPort = (operation) => requestConfirmation(operation);

  function requestConfirmation(operation: ConfirmOperation): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      // A request can only still be pending if its dialog is the one being
      // replaced here. Deny it instead of leaving its caller waiting forever.
      answerConfirm(false);
      pendingConfirm = resolve;
      confirmDialog = { isOpen: true, operation };
    });
  }

  function answerConfirm(approved: boolean): void {
    const resolve = pendingConfirm;
    pendingConfirm = null;
    resolve?.(approved);
  }

  /** Closes the Chat panel; focus goes back if the panel held it or focus fell to the body. */
  function closeChat(): void {
    const shouldReturnFocus = panelHoldsFocus('.chat-panel') || isFocusOnBody();
    isChatOpen = false;
    restoreFocusFrom('chat', shouldReturnFocus);
  }

  async function toggleChat(): Promise<void> {
    isChatOpen = !isChatOpen;
    // Re-reads the room the panel is about to show every time it opens, not
    // only the first time ever: a push that arrived while
    // the panel was closed only moved `chatRooms`, never `chatRoomStates`.
    // Nothing to select from yet on the very first open before startup's
    // room list has come back; the rail simply shows no rows.
    if (isChatOpen) {
      panelOrigins.set('chat', focusOrigin());
      void focusPanelStart('.chat-panel', '.chat-input');
      // The list itself may be stale too: participant counts follow the window
      // registry, which moves without any chat push. Awaited so
      // the default-room pick below sees the refreshed list, not a snapshot
      // still held from before this refresh fired.
      await refreshChatRooms();
      const roomId = currentRoomId === '' ? [...chatRooms.keys()][0] : currentRoomId;
      if (roomId !== undefined) {
        selectChatRoom(roomId);
      }
    } else {
      closeChat();
    }
  }

  /**
   * Reads the currently selected room (`currentRoomId`) from `afterSeq`;
   * omitted, this asks the daemon for its most recent page instead of where
   * the panel last left off. `onError` decides how a failed read is
   * surfaced. Does nothing while no room is selected.
   */
  async function refreshChat(
    afterSeq?: number,
    onError: (error: unknown) => void = (error) => reportOperationFailure(chat.panelTitle, error)
  ): Promise<void> {
    const roomId = currentRoomId;
    if (roomId === '') {
      return;
    }
    try {
      const state = chatRoomStates.get(roomId) ?? emptyChatRoom(roomId);
      const merged = mergeChatMessages(state, await invokeReadChat(roomId, afterSeq));
      // eslint-disable-next-line svelte/prefer-svelte-reactivity -- snapshot swap: a fresh Map is reassigned to the reactive `let`, so runes-based SvelteMap reactivity does not apply
      const nextStates = new Map(chatRoomStates);
      nextStates.set(roomId, merged);
      chatRoomStates = nextStates;
    } catch (error) {
      onError(error);
    }
  }

  /**
   * One room-list read: a failed one is reported to the console only, the
   * same fail-quiet rule the startup read follows, since there is no
   * obviously-relevant banner slot for a background list refresh.
   */
  async function refreshChatRoomsOnce(): Promise<void> {
    try {
      chatRooms = applyRoomsList(await invokeListChatRooms());
      chatNeedsUser = anyNeedsUser(chatRooms);
    } catch (error) {
      console.error('failed to list chat rooms:', error);
    }
  }

  /** Serialized so concurrent pushes cannot pile up `list_chat_rooms` reads. */
  const refreshChatRooms = serializeRefresh(refreshChatRoomsOnce);

  /**
   * Selects the room the right-hand column shows, and reads it every time,
   * not only the first time: a room the panel already
   * holds may have moved since it was last shown here (a push that arrived
   * while it was not the selected room, or while the panel was closed).
   * The first time a room is selected (nothing held for it in
   * `chatRoomStates` yet) it is read with the cursor omitted, the same
   * convention `refreshChat` follows everywhere else; a room already held
   * is read again from its own `nextAfterSeq`.
   */
  function selectChatRoom(roomId: string): void {
    currentRoomId = roomId;
    void refreshChat(chatRoomStates.get(roomId)?.nextAfterSeq);
  }

  /**
   * A push carries one room's state. While the panel is open the room list
   * is re-listed wholesale (`refreshChatRooms`), which is also where the
   * order, participant counts and halt marks come from; while it is closed
   * only the aggregate badge moves, with no IPC (`applyChatChangedToRooms`).
   * Either way, a push naming the room currently shown on the right may
   * still need its own page re-read.
   */
  function applyChatChanged(payload: ChatChanged): void {
    if (isChatOpen) {
      void refreshChatRooms();
      const reloadAfterSeq = chatReloadAfterSeq(
        chatRoomStates.get(currentRoomId) ?? emptyChatRoom(currentRoomId),
        payload
      );
      if (reloadAfterSeq !== null) {
        void refreshChat(reloadAfterSeq);
      }
    } else {
      chatRooms = applyChatChangedToRooms(chatRooms, payload);
      chatNeedsUser = anyNeedsUser(chatRooms);
    }
  }

  /**
   * Who takes part in the rooms changed: re-list the rooms and read the shown
   * room again, which is where the roster comes from. While the panel is
   * closed the next open reads both.
   */
  function applyChatParticipantsChanged(): void {
    if (!isChatOpen) {
      return;
    }
    void refreshChatRooms();
    void refreshChat(chatRoomStates.get(currentRoomId)?.nextAfterSeq);
  }

  /** Posts one message; `false` leaves the composer holding what was typed. */
  async function postChat(body: string, broadcast: boolean): Promise<boolean> {
    const roomId = currentRoomId === '' ? undefined : currentRoomId;
    operationError = null;
    try {
      if (broadcast) {
        await invokeBroadcastChat(body, roomId);
      } else {
        await invokeSendChat(body, roomId);
      }
    } catch (error) {
      reportOperationFailure(chat.panelTitle, error);
      return false;
    }
    await refreshChat();
    return true;
  }

  async function broadcastChat(body: string): Promise<boolean> {
    if (!(await requestConfirmation('broadcast'))) {
      return false;
    }
    return postChat(body, true);
  }

  /** One bare read of the plan list, tolerant of a failed read. */
  async function refreshPlanListOnce(): Promise<void> {
    try {
      planList = await invokeListPlans();
    } catch (error) {
      console.error('failed to list plans:', error);
    }
  }

  /** Serialized so concurrent pushes cannot pile up `list_plans` reads. */
  const refreshPlanList = serializeRefresh(refreshPlanListOnce);

  /** Closes the Plans panel; focus goes back if the panel held it or focus fell to the body. */
  function closePlans(): void {
    const shouldReturnFocus = panelHoldsFocus('.plans-panel') || isFocusOnBody();
    isPlansOpen = false;
    restoreFocusFrom('plans', shouldReturnFocus);
  }

  async function togglePlans(): Promise<void> {
    isPlansOpen = !isPlansOpen;
    // Pushes are not read while the panel is closed, so opening it reads
    // everything it shows.
    if (isPlansOpen) {
      panelOrigins.set('plans', focusOrigin());
      void focusPanelStart('.plans-panel');
      await applyPlanReload({ list: true, roles: true, planId: selectedPlanId });
      notePlanViewed();
    } else {
      closePlans();
    }
  }

  /**
   * Closes the Settings panel, the one place a switch's failure line is
   * cleared: both exits (the panel's own ✕ and pressing the rail control a
   * second time) reach it. Focus goes back if the panel held it or focus
   * fell to the body.
   */
  function closeSettings(): void {
    const shouldReturnFocus = panelHoldsFocus('.settings-panel') || isFocusOnBody();
    isSettingsOpen = false;
    profileUpdateFailure = null;
    approvalSwitchFailure = null;
    integrationResults = new Map();
    integrationsFailure = null;
    openProfileDetailId = null;
    profileDetail = null;
    profileDetailFailure = null;
    profileCommandFailure = null;
    restoreFocusFrom('settings', shouldReturnFocus);
  }

  /** The window this machine currently shows the app in, for fitting the Settings panel inside it. */
  function currentViewport(): SettingsPanelSize {
    return { width: window.innerWidth, height: window.innerHeight };
  }

  /** Fits a drag of the Settings panel's corner into the window, and remembers it as chosen. */
  function handleSettingsPanelResize(size: SettingsPanelSize): void {
    if (!isResizedByUser(size, settingsPanelSize)) {
      return;
    }
    chosenSettingsPanelSize = clampPanelSize(size, panelViewport);
    settingsPanelSizeStorage.save(chosenSettingsPanelSize);
  }

  /** Fits a drag of the Plans panel's corner into the window, and remembers it as chosen. */
  function handlePlansPanelResize(size: SettingsPanelSize): void {
    if (!isResizedByUser(size, plansPanelSize)) {
      return;
    }
    chosenPlansPanelSize = clampPanelSize(size, panelViewport);
    plansPanelSizeStorage.save(chosenPlansPanelSize);
  }

  // A smaller window only changes how the panels are shown, not the size the user chose.
  function handleWindowResize(): void {
    panelViewport = currentViewport();
  }

  async function toggleSettings(): Promise<void> {
    isSettingsOpen = !isSettingsOpen;
    if (!isSettingsOpen) {
      closeSettings();
      return;
    }
    panelOrigins.set('settings', focusOrigin());
    void focusPanelStart('.settings-panel');
    // Neither list is pushed while the panel is closed, so opening it reads
    // both again.
    await Promise.all([loadProfiles(), readRoles()]);
    // A daemon that predates the approval settings would refuse the
    // request; the tab shows the "needs a newer daemon" notice instead.
    if (isApprovalSettingsSupported(protocol_revision)) {
      try {
        approvalSettings = await invokeGetApprovalSettings();
      } catch (error) {
        console.error('failed to read approval settings:', error);
      }
    }
    await readIntegrations();
  }

  function selectSettingsTab(tab: SettingsTab): void {
    settingsTab = tab;
    void readIntegrations();
  }

  /**
   * Reads the Integrations list, only while that tab is showing and only
   * from a daemon that carries MCP registration. Every row reads as
   * "checking" again until its own status push for this read arrives.
   */
  async function readIntegrations(): Promise<void> {
    if (
      !isSettingsOpen ||
      settingsTab !== 'integrations' ||
      !isIntegrationsSupported(protocol_revision)
    ) {
      return;
    }
    const requestId = nextChoicesRequestId(lastIntegrationsRequestId);
    lastIntegrationsRequestId = requestId;
    integrationsListRequestId = requestId;
    integrationStatuses = new Map();
    integrationsFailure = null;
    try {
      const listed = await invokeListMcpRegistrations(requestId);
      if (integrationsListRequestId === requestId) {
        integrationsListed = listed;
      }
    } catch (error) {
      if (integrationsListRequestId === requestId) {
        integrationsFailure = describeOperationFailure(settings.integrationsListFailure, error);
      }
    }
  }

  function applyMcpRegistrationStatus(payload: McpRegistrationStatusPayload): void {
    if (payload.requestId !== integrationsListRequestId) {
      return;
    }
    // eslint-disable-next-line svelte/prefer-svelte-reactivity -- snapshot swap: a fresh Map is reassigned to the reactive `let`, so runes-based SvelteMap reactivity does not apply
    integrationStatuses = new Map(integrationStatuses).set(payload.status.profile, payload.status);
  }

  /**
   * Registers, updates or removes one profile's MCP registration. The row
   * shows as operating from before the request is sent until its result
   * push arrives, or until the request itself is refused.
   */
  async function runMcpRegistration(profile: string, action: McpRegistrationAction): Promise<void> {
    if (integrationRunning !== null) {
      return;
    }
    const requestId = nextChoicesRequestId(lastIntegrationsRequestId);
    lastIntegrationsRequestId = requestId;
    integrationsRunRequestId = requestId;
    integrationRunning = profile;
    integrationsFailure = null;
    try {
      await invokeRunMcpRegistration(profile, action, requestId);
    } catch (error) {
      if (integrationsRunRequestId === requestId) {
        integrationsRunRequestId = null;
        integrationRunning = null;
        const name =
          integrationsListed?.rows.find((row) => row.profile === profile)?.displayName ?? profile;
        integrationsFailure = describeOperationFailure(settings.integrationRunFailure(name), error);
      }
    }
  }

  function applyMcpRegistrationResult(payload: McpRegistrationResultPayload): void {
    if (payload.requestId !== integrationsRunRequestId) {
      return;
    }
    integrationsRunRequestId = null;
    // eslint-disable-next-line svelte/prefer-svelte-reactivity -- snapshot swap: a fresh Map is reassigned to the reactive `let`, so runes-based SvelteMap reactivity does not apply
    integrationStatuses = new Map(integrationStatuses).set(payload.profile, payload.status);
    // eslint-disable-next-line svelte/prefer-svelte-reactivity -- snapshot swap: a fresh Map is reassigned to the reactive `let`, so runes-based SvelteMap reactivity does not apply
    integrationResults = new Map(integrationResults).set(payload.profile, payload);
    integrationRunning = null;
  }

  /**
   * Switches one gated tool's auto-approve on or off from the Approvals tab.
   * Refused, the switch is left as it was and the tab shows why; either way
   * every switch on the tab stops showing as saving.
   */
  async function handleSetAutoApprove(tool: string, autoApprove: boolean): Promise<void> {
    isApprovalSaving = true;
    approvalSwitchFailure = null;
    try {
      approvalSettings = await invokeSetAutoApprove(tool, autoApprove);
    } catch (error) {
      const reason = foldExternalText(
        typeof error === 'string' ? error : error instanceof Error ? error.message : String(error)
      );
      // The request asked for `autoApprove`; a refusal leaves the tool at
      // the opposite of that.
      approvalSwitchFailure = approvalUpdateFailure(tool, reason, !autoApprove, 'tool');
    } finally {
      isApprovalSaving = false;
    }
  }

  /**
   * Switches the plan-review gate on or off from the Approvals tab. Refused,
   * the switch is left as it was and the tab shows why; either way every
   * switch on the tab stops showing as saving.
   */
  async function handleSetPlanApprovalRequired(required: boolean): Promise<void> {
    isApprovalSaving = true;
    approvalSwitchFailure = null;
    try {
      approvalSettings = await invokeSetPlanApprovalRequired(required);
    } catch (error) {
      const reason = foldExternalText(
        typeof error === 'string' ? error : error instanceof Error ? error.message : String(error)
      );
      // The request asked for `required`; a refusal leaves the gate at the
      // opposite of that.
      approvalSwitchFailure = approvalUpdateFailure(PLAN_REVIEW_GATE_ID, reason, !required, 'plan');
    } finally {
      isApprovalSaving = false;
    }
  }

  /**
   * Switches one launch profile on or off from the Settings panel. Refused,
   * the switch is left as it was and the panel shows why; either way the row
   * stops showing as saving.
   */
  async function handleSetProfileEnabled(id: string, enabled: boolean): Promise<void> {
    savingProfileId = id;
    profileUpdateFailure = null;
    try {
      await invokeSetProfileEnabled(id, enabled);
      await loadProfiles();
    } catch (error) {
      const reason = foldExternalText(
        typeof error === 'string' ? error : error instanceof Error ? error.message : String(error)
      );
      // The request asked for `enabled`; a refusal leaves the profile at the
      // opposite of that.
      profileUpdateFailure = settings.profileUpdateFailed(id, reason, !enabled);
    } finally {
      savingProfileId = null;
    }
  }

  /**
   * A `plan_changed` push always refreshes the list -- and with it the
   * plans-attention badge -- even while the panel is closed:
   * the push only carries a plan id, not the summary fields the badge reads,
   * so this is not something `isPlansOpen`'s local state can answer without
   * a read.
   */
  function applyPlanChanged(payload: PlanChanged): void {
    void refreshPlanList();
    if (!isPlansOpen) {
      return;
    }
    void applyPlanReload(
      planReloadFor({ kind: 'plan-changed', planId: payload.plan_id }, selectedPlanId)
    );
  }

  function selectPlan(planId: string): void {
    selectedPlanId = planId;
    plan = null;
    retainedMarks = {};
    void readPlan(planId).then(() => {
      if (selectedPlanId === planId) {
        notePlanViewed();
      }
    });
  }

  /**
   * Tells the daemon the user has looked at the selected plan's marked
   * tasks, but only when opening it is what "seen" means here:
   * a push re-reading the plan is not a look.
   */
  function notePlanViewed(): void {
    if (plan !== null && shouldMarkPlanSeen('open', plan)) {
      void markPlanSeen();
    }
  }

  async function markPlanSeen(): Promise<void> {
    const planId = selectedPlanId;
    if (planId === null || plan === null) {
      return;
    }
    retainedMarks = { ...retainedMarks, ...reviewMarksOf(plan) };
    try {
      const next = await invokeMarkPlanSeen(planId, plan.version);
      if (selectedPlanId === planId) {
        plan = next;
      }
    } catch (error) {
      console.error('failed to mark the plan seen:', error);
    }
  }

  /**
   * Approves the selected plan. A refusal -- most often a stale `version`,
   * since the daemon rejects the approval if the tasks changed since it was
   * last read -- rereads the plan so the board shows the current one.
   */
  async function approvePlan(): Promise<void> {
    const planId = selectedPlanId;
    if (planId === null || plan === null) {
      return;
    }
    operationError = null;
    try {
      const next = await invokeApprovePlan(planId, plan.version);
      if (selectedPlanId === planId) {
        plan = next;
      }
    } catch (error) {
      reportOperationFailure(nav.plans, error);
      await readPlan(planId);
    }
  }

  async function saveRoles(next: RoleEntry[], nextDefaultRole: string | null): Promise<void> {
    operationError = null;
    try {
      ({ roles, default_role: defaultRole } = await invokeSaveRoles(next, nextDefaultRole));
    } catch (error) {
      reportOperationFailure(nav.plans, error);
      return;
    }
    // The table decides which assignment each task resolves to, so the
    // shown plan is stale as well.
    await applyPlanReload(planReloadFor({ kind: 'roles-saved' }, selectedPlanId));
  }

  async function readRoles(): Promise<void> {
    try {
      ({ roles, default_role: defaultRole } = await invokeListRoles());
    } catch (error) {
      reportOperationFailure(nav.plans, error);
    }
  }

  function selectModelProfile(profile: string): void {
    if (!Object.prototype.hasOwnProperty.call(shippedModels, profile)) {
      void loadShippedAliases(profile);
    }
    if (modelChoices.has(profile)) {
      return;
    }
    void requestModelChoices(profile);
  }

  async function loadShippedAliases(profile: string): Promise<void> {
    try {
      const detail = await invokeGetProfileDetail(profile);
      shippedModels = { ...shippedModels, [profile]: { aliases: shippedAliasesOf(detail) } };
    } catch (error) {
      console.error('failed to read the shipped model aliases:', error);
    }
  }

  function refreshModelChoices(profile: string): void {
    void requestModelChoices(profile);
  }

  async function requestModelChoices(profile: string): Promise<void> {
    const requestId = nextChoicesRequestId(lastChoicesRequestId);
    lastChoicesRequestId = requestId;
    latestChoicesRequestIds.set(profile, requestId);
    modelChoices = withModelChoicesEntry(profile, { kind: 'fetching', startedAtMs: Date.now() });
    setTimeout(() => expireModelChoicesRequest(profile, requestId), CHOICES_FETCH_TIMEOUT_MS);
    try {
      await invokeListModelChoices(profile, requestId);
    } catch (error) {
      if (latestChoicesRequestIds.get(profile) === requestId) {
        modelChoices = withModelChoicesEntry(profile, {
          kind: 'failed',
          failure: { kind: 'reported', reason: String(error) },
        });
      }
    }
  }

  function applyModelChoices(payload: ModelChoicesPayload): void {
    if (!acceptsChoicesPush(latestChoicesRequestIds, payload)) {
      return;
    }
    modelChoices = withModelChoicesEntry(payload.profile, {
      kind: 'answered',
      source: payload.source,
      choices: payload.choices,
      error: payload.error,
      fetchedAtMs: Date.now(),
    });
  }

  function expireModelChoicesRequest(profile: string, requestId: number): void {
    const entry = modelChoices.get(profile);
    if (latestChoicesRequestIds.get(profile) !== requestId || entry?.kind !== 'fetching') {
      return;
    }
    if (!isChoicesFetchOverdue(entry.startedAtMs, Date.now())) {
      // A timer may fire a little early; look again shortly.
      setTimeout(() => expireModelChoicesRequest(profile, requestId), OVERDUE_RECHECK_MS);
      return;
    }
    modelChoices = withModelChoicesEntry(profile, {
      kind: 'failed',
      failure: { kind: 'overdue' },
    });
  }

  function withModelChoicesEntry(
    profile: string,
    entry: ModelChoicesEntry
  ): ReadonlyMap<string, ModelChoicesEntry> {
    // eslint-disable-next-line svelte/prefer-svelte-reactivity -- snapshot swap: a fresh Map is reassigned to the reactive `let`, so runes-based SvelteMap reactivity does not apply
    return new Map(modelChoices).set(profile, entry);
  }

  async function saveModelAliases(profile: string, aliases: LaunchOptionAlias[]): Promise<void> {
    operationError = null;
    try {
      await invokeSaveModelAliases(profile, aliases);
    } catch (error) {
      reportOperationFailure(nav.plans, error);
      return;
    }
    // The editor takes the saved values from the profile list.
    await loadProfiles();
  }

  function errorReason(error: unknown): string {
    return foldExternalText(
      typeof error === 'string' ? error : error instanceof Error ? error.message : String(error)
    );
  }

  /** Reads the open profile's details, dropping an answer for one since closed. */
  async function loadProfileDetail(id: string): Promise<void> {
    try {
      const detail = await invokeGetProfileDetail(id);
      if (openProfileDetailId === id) {
        profileDetail = detail;
        profileDetailFailure = null;
      }
    } catch (error) {
      if (openProfileDetailId === id) {
        profileDetailFailure = errorReason(error);
      }
    }
  }

  function toggleProfileDetail(id: string): void {
    profileDetail = null;
    profileDetailFailure = null;
    profileCommandFailure = null;
    openProfileDetailId = openProfileDetailId === id ? null : id;
    if (openProfileDetailId !== null) {
      void loadProfileDetail(openProfileDetailId);
    }
  }

  /**
   * Sets the open profile's command, or with `argv` null puts the shipped one
   * back. The listing and the details are read again either way, so the row
   * shows what the daemon now uses.
   */
  async function saveProfileCommand(id: string, argv: string[] | null): Promise<void> {
    savingCommandProfileId = id;
    profileCommandFailure = null;
    try {
      await invokeSetProfileCommand(id, argv);
    } catch (error) {
      profileCommandFailure = { id, reason: errorReason(error) };
    } finally {
      savingCommandProfileId = null;
    }
    await Promise.all([loadProfiles(), loadProfileDetail(id)]);
  }

  /** Takes the user from a profile's details to its aliases in Plans > Models. */
  async function openModelsFor(id: string): Promise<void> {
    // The two panels share a layer, so Settings left open would cover Plans.
    closeSettings();
    if (!isPlansOpen) {
      await togglePlans();
    }
    requestedModelProfile = { id, seq: (requestedModelProfile?.seq ?? 0) + 1 };
  }

  async function copyText(text: string): Promise<void> {
    try {
      await writeClipboardText(text);
    } catch (error) {
      console.error('failed to copy to the clipboard:', error);
    }
  }

  const profileDetailActions: ProfileDetailActions = {
    onToggle: toggleProfileDetail,
    onSaveCommand: (id, argv) => void saveProfileCommand(id, argv),
    onResetCommand: (id) => void saveProfileCommand(id, null),
    onOpenModels: (id) => void openModelsFor(id),
    onCopyText: (text) => void copyText(text),
  };

  async function applyPlanReload(reload: PlanReload): Promise<void> {
    await Promise.all([
      reload.list ? readPlanList() : Promise.resolve(),
      reload.roles ? readRoles() : Promise.resolve(),
      reload.planId === null ? Promise.resolve() : readPlan(reload.planId),
    ]);
  }

  async function readPlanList(): Promise<void> {
    try {
      planList = await invokeListPlans();
    } catch (error) {
      reportOperationFailure(nav.plans, error);
      return;
    }
    const first = planList[0];
    if (selectedPlanId === null && first !== undefined) {
      selectPlan(first.plan_id);
    }
  }

  /** Reads one plan; the answer is dropped if another plan was selected meanwhile. */
  async function readPlan(planId: string): Promise<void> {
    try {
      const next = await invokeGetPlan(planId);
      if (selectedPlanId === planId) {
        plan = next;
      }
    } catch (error) {
      const remaining = selectionAfterPlanReadFailure(selectedPlanId, planId, error);
      if (remaining !== selectedPlanId) {
        selectedPlanId = remaining;
        plan = null;
        void readPlanList();
      } else if (remaining === planId) {
        reportOperationFailure(nav.plans, error);
      }
    }
  }

  async function deletePlan(planId: string): Promise<void> {
    operationError = null;
    try {
      await invokeDeletePlan(planId);
    } catch (error) {
      reportOperationFailure(nav.plans, error);
      return;
    }
    if (selectedPlanId === planId) {
      selectedPlanId = null;
      plan = null;
    }
    await readPlanList();
  }

  async function setTaskAssignment(taskId: string, assignment: TaskAssignment): Promise<void> {
    const planId = selectedPlanId;
    if (planId === null) {
      return;
    }
    operationError = null;
    try {
      const next = await invokeSetTaskAssignment(planId, taskId, assignment);
      if (selectedPlanId === planId) {
        plan = next;
      }
    } catch (error) {
      reportOperationFailure(nav.plans, error);
    }
  }

  async function clearTaskAssignment(taskId: string): Promise<void> {
    const planId = selectedPlanId;
    if (planId === null) {
      return;
    }
    operationError = null;
    try {
      const next = await invokeClearTaskAssignment(planId, taskId);
      if (selectedPlanId === planId) {
        plan = next;
      }
    } catch (error) {
      reportOperationFailure(nav.plans, error);
    }
  }

  /** Resolves false when the daemon refused the window; the panel says so itself. */
  async function setPlanOwner(windowId: string): Promise<boolean> {
    const planId = selectedPlanId;
    if (planId === null) {
      return false;
    }
    try {
      const next = await invokeSetPlanOwner(planId, windowId);
      if (selectedPlanId === planId) {
        plan = next;
      }
      return true;
    } catch (error) {
      console.error('failed to assign the plan owner:', error);
      return false;
    }
  }

  /**
   * Whether the window that submitted `request` is still in the polled window
   * list, which is what tells the two "window" requester displays apart.
   */
  function isRequesterWindowOpen(request: ApprovalRequest | null, windows: WindowEntry[]): boolean {
    const requesterId = request?.requester.window_id ?? null;
    return requesterId !== null && windows.some((entry) => entry.windowId === requesterId);
  }

  /**
   * Notes which request the dialog is showing and when it started showing it.
   * The click guard restarts on every opening, a reopening behind a modal
   * that was in front of it included, because that is just as sudden.
   */
  function syncApprovalDisplay(isDialogOpen: boolean, request: ApprovalRequest | null): void {
    const displayedId = isDialogOpen && request !== null ? request.request_id : null;
    if (displayedId === approvalDisplayedId) {
      return;
    }
    approvalDisplayedId = displayedId;
    if (displayedId === null) {
      return;
    }
    approvalOpenedAt = Date.now();
    approvals = approvalQueue.markShown(approvals, displayedId);
  }

  /**
   * Drops a request the daemon withdrew. One that expired before it was ever
   * displayed is counted for the banner instead of disappearing without the
   * user ever learning it was asked -- but only when it actually timed out:
   * a decision made elsewhere, or the requester
   * disconnecting, is not an unnoticed expiry.
   */
  function withdrawApproval(requestId: string, reason: WithdrawReason): void {
    const request = approvals.items.find((entry) => entry.request_id === requestId);
    if (request !== undefined) {
      // `bannerState` is only recomputed at the next flush, so the live field
      // is spread over it: two withdrawals in one tick both have to count.
      approvalExpiredUnseen = recordApprovalExpiredUnseen(
        { ...bannerState, approvalExpiredUnseen },
        approvalQueue.wasShown(approvals, requestId),
        reason,
        {
          requester: sanitizeForDisplay(
            request.requester.name ?? dialogs.approvalRequesterExternal
          ),
          tool: request.tool,
          target:
            request.target !== undefined
              ? sanitizeForDisplay(request.target.name)
              : describeUntargeted(request),
        }
      ).approvalExpiredUnseen;
    }
    approvals = approvalQueue.withdraw(approvals, requestId);
  }

  /**
   * Sends the user's decision. The request is marked in flight rather than
   * removed optimistically, so a failed round trip can offer it again
   * instead of it vanishing from the queue while its fate is unknown.
   */
  function decideApproval(requestId: string, approved: boolean): void {
    approvals = approvalQueue.markInFlight(approvals, requestId);
    void invokeDecideApproval(requestId, approved)
      .then((outcome) => {
        // Both settle the request from this queue's point of view: 'decided'
        // is this decision taking effect, 'not_pending' is it losing a race
        // with one that already did (or a timeout) -- neither is a failure.
        switch (outcome) {
          case 'decided':
          case 'not_pending':
            approvals = approvalQueue.resolve(approvals, requestId);
            break;
        }
      })
      .catch((error: unknown) => {
        console.error('failed to decide approval:', error);
        approvals = approvalQueue.clearInFlight(approvals, requestId);
        operationError = banners.approvalDecisionFailed;
      });
  }

  /** Whether some window is currently maximized: tab mode has no state of its own. */
  function isTabMode(): boolean {
    return get(registry).windows.some((entry) => entry.display.isMaximized);
  }

  /**
   * Switches to a tab: maximizes `windowId` and raises it, in that order. Shared by the tab strip's click
   * handler, `window-actions.focus`/`minimize` and the
   * new-window-while-in-tab-mode case below.
   *
   * A minimized window is restored first, so clicking its dimmed tab brings it
   * back instead of switching to an empty float area.
   */
  function switchToTab(windowId: string): void {
    registry.setWindowMinimized(windowId, false);
    registry.setWindowMaximized(windowId, true);
    registry.bringWindowToFront(windowId);
  }

  /**
   * A tab is a valid successor only while it is open and not minimized: a
   * minimized tab would put the strip right back where the user just
   * collapsed it from.
   */
  function isEligibleTabSuccessor(windowId: string): boolean {
    const entry = get(registry).windows.find((candidate) => candidate.windowId === windowId);
    return entry !== undefined && !entry.display.isMinimized;
  }

  /** Resolves the tab `window-actions.minimize` should switch to. */
  function resolveTabSuccessor(windowId: string): string | null {
    return nextActiveTab(get(registry).tabOrder, isEligibleTabSuccessor, windowId);
  }

  /** The tab strip's WindowControls acts on the active (maximized) window. */
  function handleTabRename(): void {
    if (activeTabEntry !== undefined) {
      handleRenameWindow(activeTabEntry);
    }
  }

  const contractedActions = createWindowActions({
    registry,
    ipc: { invokeCloseWindow, invokeInterruptWindow, invokeRenameWindow },
    confirmPort,
    removeWindow: closePane,
    isTabMode,
    switchToTab,
    resolveTabSuccessor,
  });

  // The view starts these operations without awaiting them, so a refusal has
  // to become a banner here, where the banner is owned.
  const actions: WindowActions = {
    ...contractedActions,
    close: (entry) => reportFailures(errors.closeWindowFailed, contractedActions.close(entry)),
    interrupt: (entry) =>
      reportFailures(errors.interruptWindowFailed, contractedActions.interrupt(entry)),
    rename: (entry, name) =>
      reportFailures(errors.renameWindowFailed, contractedActions.rename(entry, name)),
  };

  // The jump to the next window needing the user reads the rooms as the sidebar
  // renders them, and expands through the store's idempotent `expand` so a room
  // already open is never closed.
  const attentionNavigation = createAttentionNavigation({
    groups: sidebarGroups,
    expandRoom: collapsedRooms.expand,
    focus: actions.focus,
    focusTerminal: focusTerminalOf,
  });

  const FOCUSABLE =
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  let sidebarElement: HTMLElement | null = null;

  function firstFocusable(
    root: ParentNode | null,
    selector: string = FOCUSABLE
  ): HTMLElement | null {
    return root?.querySelector<HTMLElement>(selector) ?? null;
  }

  function sidebarRow(windowId: string): HTMLElement | null {
    return firstFocusable(sidebarElement, `[data-window-id="${CSS.escape(windowId)}"]`);
  }

  /** Whether any modal dialog is open; the DOM is the only record of it. */
  function hasOpenModal(): boolean {
    return document.querySelector('dialog:modal') !== null;
  }

  function runFocusMove(move: FocusMove): void {
    switch (move.kind) {
      case 'terminal':
        get(panes).get(move.windowId)?.controller.focusTerminal();
        return;
      case 'sidebar-row':
        sidebarRow(move.windowId)?.focus();
        return;
      case 'sidebar-start':
        firstFocusable(sidebarElement)?.focus();
        return;
      case 'none':
        return;
    }
  }

  // Every focus move goes through one held request that runs once no modal
  // dialog is open, after the view has rendered (a hidden window cannot take focus).
  const pendingFocus = createPendingFocus({
    isModalOpen: hasOpenModal,
    settle: tick,
    run: runFocusMove,
  });

  /** Asks for keyboard focus to move into a window's terminal. */
  function focusTerminalOf(windowId: string): void {
    pendingFocus.request({ kind: 'terminal', windowId });
  }

  function moveFocus(move: FocusMove): void {
    if (move.kind === 'terminal') {
      focusTerminalOf(move.windowId);
    } else {
      pendingFocus.request(move);
    }
  }

  /** Returns keyboard focus to the current window, or to the sidebar when there is none. */
  function restoreFocus(): void {
    moveFocus(restoreFocusMove(currentWindow(get(registry).windows)));
  }

  /** Moves focus between the sidebar and the current window; it depends only on where focus is now. */
  function toggleSidebarFocus(): void {
    const focusInSidebar = sidebarElement?.contains(document.activeElement) ?? false;
    moveFocus(
      sidebarToggleMove(
        focusInSidebar,
        currentWindow(get(registry).windows),
        (windowId: string) => sidebarRow(windowId) !== null
      )
    );
  }

  /** The sidebar's rooms as it renders them, top to bottom. */
  function sidebarGroups(): RoomGroup<WindowEntry>[] {
    return groupByRoom(get(registry).windows, get(collapsedRooms), get(registry).tabOrder);
  }

  const runShortcut = createShortcutRunner<WindowEntry>({
    windows: () => get(registry).windows,
    displayOrder: () => windowsInDisplayOrder(sidebarGroups()),
    groups: sidebarGroups,
    expandRoom: collapsedRooms.expand,
    actions,
    goToNextWindowNeedingYou: attentionNavigation.goToNextWindowNeedingYou,
    focusTerminal: focusTerminalOf,
    restoreFocus,
    openWindowForm: openAddDialog,
    toggleChat,
    togglePlans,
    toggleSettings,
    toggleSidebarFocus,
  });

  // The window-level capture listener below is the only place keys are judged as
  // shortcuts, so it sees every key before the terminal and the inputs do.
  const shortcutDispatcher = createShortcutDispatcher({
    bindings: () => get(keymapStore).bindings,
    isModalOpen: hasOpenModal,
    run: (action) => {
      void reportIfFailed(errors.shortcutFailed, runShortcut(action));
    },
    platform: keyboardPlatform,
  });

  function panelHoldsFocus(panelSelector: string): boolean {
    return document.querySelector(panelSelector)?.contains(document.activeElement) ?? false;
  }

  /** Whether focus sits on the body, as it does after a focused button inside a panel is disabled. */
  function isFocusOnBody(): boolean {
    return document.activeElement === null || document.activeElement === document.body;
  }

  function isPanelOpen(panel: PanelId): boolean {
    switch (panel) {
      case 'chat':
        return isChatOpen;
      case 'plans':
        return isPlansOpen;
      case 'settings':
        return isSettingsOpen;
    }
  }

  function closePanel(panel: PanelId): void {
    switch (panel) {
      case 'chat':
        closeChat();
        return;
      case 'plans':
        closePlans();
        return;
      case 'settings':
        closeSettings();
        return;
    }
  }

  function panelContaining(node: EventTarget | null): PanelId | null {
    if (!(node instanceof Element)) {
      return null;
    }
    return PANEL_IDS.find((panel) => node.closest(PANEL_SELECTORS[panel]) !== null) ?? null;
  }

  /** Where focus is now, remembered by a panel that is opening. */
  function focusOrigin(): PanelOrigin<PanelId, HTMLElement> {
    const element = document.activeElement;
    if (!(element instanceof HTMLElement) || element === document.body) {
      return { kind: 'other' };
    }
    const panel = panelContaining(element);
    if (panel !== null) {
      return { kind: 'panel', panel, element };
    }
    const isSidebarRow =
      (sidebarElement?.contains(element) ?? false) && element.closest('[data-window-id]') !== null;
    return isSidebarRow || element.closest('.terminal-pane') !== null
      ? { kind: 'carried' }
      : { kind: 'other' };
  }

  function notePanelFocus(event: FocusEvent): void {
    const panel = panelContaining(event.target);
    if (panel !== null) {
      lastFocusedPanel = panel;
      recentPanels = [panel, ...recentPanels.filter((open) => open !== panel)];
    } else if (event.target !== document.body) {
      lastFocusedPanel = null;
    }
  }

  /**
   * Forgets `panel`, which has just closed, and when `shouldReturnFocus` sends
   * focus back to where the panel was opened from.
   */
  function restoreFocusFrom(panel: PanelId, shouldReturnFocus: boolean): void {
    const target = panelReturnTarget<PanelId, HTMLElement>({
      origin: panelOrigins.get(panel) ?? { kind: 'other' },
      closing: panel,
      isOpen: isPanelOpen,
      isConnected: (element: HTMLElement) => element.isConnected,
      recentPanels,
      current: currentWindow(get(registry).windows),
    });
    panelOrigins.delete(panel);
    recentPanels = recentPanels.filter((open) => open !== panel);
    if (lastFocusedPanel === panel) {
      lastFocusedPanel = null;
    }
    if (!shouldReturnFocus) {
      return;
    }
    switch (target.kind) {
      case 'element':
        target.element.focus();
        return;
      case 'panel':
        void focusPanelStart(
          PANEL_SELECTORS[target.panel],
          target.panel === 'chat' ? '.chat-input' : FOCUSABLE
        );
        return;
      case 'move':
        moveFocus(target.move);
        return;
    }
  }

  /** The window-level capture listener: shortcuts first, then the Escape no panel received. */
  function handleWindowKeydown(event: KeyboardEvent): void {
    shortcutDispatcher.handleKeydown(event);
    const panel = bodyEscapePanel(event, {
      focusOnBody: isFocusOnBody(),
      isModalOpen: hasOpenModal(),
      lastFocusedPanel,
      isOpen: isPanelOpen,
    });
    if (panel !== null) {
      event.preventDefault();
      closePanel(panel);
    }
  }

  /** Moves focus into a panel that is opening, once it has rendered. */
  async function focusPanelStart(
    panelSelector: string,
    targetSelector: string = FOCUSABLE
  ): Promise<void> {
    await tick();
    firstFocusable(document.querySelector(panelSelector), targetSelector)?.focus();
  }

  /** Clears the banner, then shows the operation's failure in it if it fails. */
  async function reportFailures(summary: string, operation: Promise<void>): Promise<void> {
    operationError = null;
    await reportIfFailed(summary, operation);
  }

  // Leaves a banner already on screen alone, so a shortcut that succeeds does not
  // dismiss an unrelated error.
  async function reportIfFailed(summary: string, operation: Promise<void>): Promise<void> {
    try {
      await operation;
    } catch (error) {
      reportOperationFailure(summary, error);
    }
  }

  onDestroy(() => {
    unbindAppPalette();
    stopLayoutSync();
    unsubscribeSession?.();
    void framePull?.stop();
    unsubscribeWindowList?.();
    unsubscribeChatChanged?.();
    unsubscribeChatParticipantsChanged?.();
    unsubscribePlanChanged?.();
    unsubscribeModelChoices?.();
    unsubscribeMcpRegistrationStatus?.();
    unsubscribeMcpRegistrationResult?.();
    for (const windowId of [...get(panes).keys()]) {
      closePane(windowId);
    }
  });

  // Initialize: subscribe to events, load the window list, then pull the
  // pending approvals. The subscription comes first so that nothing the core
  // emits while the list is loading is missed, and the pull comes last so a
  // daemon that has to be waited out (ATTACH_TIMEOUT) cannot delay it.
  async function initialize(): Promise<void> {
    try {
      unsubscribeSession = await subscribeSession(registry.resolveWindowHandlers, {
        onStatus: (payload) => {
          registry.setStatus(payload.window_id, payload.detected, payload.status_rev ?? null);
        },
        onDaemonSpawnWarning: (payload) => {
          daemonSpawnNotice = foldExternalText(payload.message);
        },
        onBoundaryError: (error) => {
          reportBoundaryNotice('sessionEvent', error);
        },
        onApprovalRequested: (payload) => {
          approvals = approvalQueue.enqueue(approvals, payload.request);
        },
        onApprovalWithdrawn: (payload) => {
          withdrawApproval(payload.request_id, payload.reason);
        },
        onApprovalReset: (payload) => {
          approvals = approvalQueue.reset(approvals, payload.requests);
          approverRegistrationRefused = false;
        },
        onApprovalRegistrationRefused: () => {
          approverRegistrationRefused = true;
        },
        onDaemonRecovery: (payload) => {
          const updated = applyRecoveryUpdate(
            { ...bannerState, daemonRecovery, daemonRecoveryRev, isDaemonRecoveryDismissed },
            payload
          );
          daemonRecovery = updated.daemonRecovery;
          daemonRecoveryRev = updated.daemonRecoveryRev;
          isDaemonRecoveryDismissed = updated.isDaemonRecoveryDismissed;
        },
      });
    } catch (error) {
      // A subscription that could not be opened must not take the window list
      // down with it: the poll still refreshes the list without events.
      reportBoundaryNotice('sessionEvents', error);
    }

    // One receiving loop for the frames of every window, started with the
    // subscription: a frame the core queues before the loop asks is kept.
    framePull = startFramePull({
      pull: (acks) =>
        invokeNextFrames(acks, (error) => {
          reportBoundaryNotice('sessionEvent', error);
        }),
      deliver: (windowId, frame) => registry.resolveWindowHandlers(windowId)?.onFrame(frame),
      onError: (error) => {
        reportBoundaryNotice('sessionEvents', error);
      },
    });

    // Pulled right after the subscription opens: a status the
    // supervisor published before this subscription existed would otherwise
    // be lost, the same reasoning the approval sync pull below follows.
    try {
      const report = await invokeGetRecoveryStatus();
      const updated = applyRecoveryUpdate(
        { ...bannerState, daemonRecovery, daemonRecoveryRev, isDaemonRecoveryDismissed },
        { status: report.status, rev: report.rev }
      );
      daemonRecovery = updated.daemonRecovery;
      daemonRecoveryRev = updated.daemonRecoveryRev;
      isDaemonRecoveryDismissed = updated.isDaemonRecoveryDismissed;
      const withFailure = applyClientConfigFailure(
        { ...bannerState, clientConfigFailure },
        report.config_failure
      );
      clientConfigFailure = withFailure.clientConfigFailure;
      startupEnvFailure = report.startup_env_failure;
    } catch (error) {
      console.error('failed to pull the daemon recovery status:', error);
    }

    try {
      daemonHealthNotices = daemonProblemNotices(await invokeGetDaemonHealth());
    } catch (error) {
      console.error('failed to pull the daemon health:', error);
    }

    await refreshWindowList();

    unsubscribeWindowList = await subscribeWindowList(
      (payload) => {
        protocol_revision = payload.protocol_revision;
        isDaemonProtocolStale =
          protocol_revision !== null && protocol_revision < ATTACH_VIEW_MIN_REVISION;
        isModelAliasesReadOnly =
          protocol_revision !== null && protocol_revision < MODEL_ALIASES_MIN_REVISION;
        void applyWindowList(payload.windows);
      },
      (error) => {
        reportBoundaryNotice('windowList', error);
      }
    );

    unsubscribeChatChanged = await subscribeChatChanged(applyChatChanged, (error) => {
      reportBoundaryNotice('chatChanged', error);
    });

    unsubscribeChatParticipantsChanged = await subscribeChatParticipantsChanged(
      applyChatParticipantsChanged,
      (error) => {
        reportBoundaryNotice('chatChanged', error);
      }
    );

    // A dropped plan push leaves the board at most one push behind, and the
    // next push or reopening the panel reads it again.
    unsubscribePlanChanged = await subscribePlanChanged(applyPlanChanged, (error) => {
      console.error('planChanged boundary error:', error);
    });

    // A dropped model-list push leaves that profile fetching until the
    // request is overdue, which the tab reports with a refresh beside it.
    unsubscribeModelChoices = await listenModelChoices(applyModelChoices, (error) => {
      console.error('modelChoices boundary error:', error);
    });

    // A dropped status push leaves its row checking until the tab is shown
    // again; a dropped result push leaves the row operating until then too.
    unsubscribeMcpRegistrationStatus = await listenMcpRegistrationStatus(
      applyMcpRegistrationStatus,
      (error) => {
        console.error('mcpRegistrationStatus boundary error:', error);
      }
    );
    unsubscribeMcpRegistrationResult = await listenMcpRegistrationResult(
      applyMcpRegistrationResult,
      (error) => {
        console.error('mcpRegistrationResult boundary error:', error);
      }
    );

    try {
      const outcome = await invokeSyncApprovals();
      if (outcome.status === 'registered') {
        approvals = approvalQueue.merge(approvals, outcome.pending);
        approverRegistrationRefused = false;
      } else {
        approverRegistrationRefused = true;
      }
    } catch (error) {
      // Keeping whatever the events already delivered: with no daemon to pull
      // from, the poll's own registration emits the list once one is running.
      console.error('failed to pull the pending approvals:', error);
    }

    // Seeds the room list and the aggregate badge from the daemon's state as
    // of connect time: a `chat_changed` push only ever carries a later
    // state, so without this read the badge would stay dark until the first
    // push. Read directly rather than through `refreshChatRooms`: nothing
    // else can be racing this, the very first room-list read of the run.
    try {
      chatRooms = applyRoomsList(await invokeListChatRooms());
      chatNeedsUser = anyNeedsUser(chatRooms);
    } catch (error) {
      console.error('failed to list chat rooms at startup:', error);
    }

    // Seeds the plan list -- and with it the plans-attention badge -- from
    // the daemon's state as of connect time, the same reason chatRooms is
    // seeded just above: a `plan_changed` push only ever carries a later
    // state.
    try {
      planList = await invokeListPlans();
    } catch (error) {
      console.error('failed to list plans at startup:', error);
    }
  }

  /**
   * Loads the external keymap into the keymap store, which combines it with
   * the built-in defaults. No key press reaches an action yet. Each
   * entry of an old chord-to-action table counts as discarded.
   *
   * A missing file is the normal way of running on the defaults, so it is the
   * one outcome that stays silent.
   */
  async function loadKeymap(): Promise<void> {
    try {
      const loaded = await invokeLoadKeymap();
      keymapLoadStatus = { kind: 'loaded', result: loaded };
      if (loaded.failure !== undefined && loaded.failure !== 'not_found') {
        keymapNotice = banners.keymapLoadFailed;
        return;
      }
      const { discarded } = keymapStore.load(loaded);
      if (discarded > 0) {
        keymapNotice = describeDiscardedKeymapEntries(discarded);
      }
    } catch (error) {
      console.error('failed to load keymap:', error);
      keymapLoadStatus = { kind: 'rejected' };
      keymapNotice = banners.keymapLoadFailed;
    }
  }

  /** Asks which keymap.toml this instance reads, so the Keyboard tab can name it. */
  async function loadKeymapFilePath(): Promise<void> {
    try {
      keymapFilePath = await invokeKeymapFilePath();
    } catch (error) {
      console.error('failed to read the keymap file path:', error);
    }
  }

  /**
   * Loads the user's themes and hands them to the store, once at startup.
   * A missing, unreadable or malformed file is reported through
   * the result, so only a reply that fails validation lands in the catch.
   */
  async function loadThemes(): Promise<void> {
    try {
      themeStore.loadUserThemes(await invokeLoadThemes());
    } catch (error) {
      reportBoundaryNotice('themes', error);
    }
  }

  /**
   * Refreshes the detection definition names the dropdown offers: once after
   * startup and again whenever the dropdown takes focus, so a definition added
   * while the application runs shows up without a restart.
   *
   * Both ways of failing -- a reply that fails validation and a daemon that
   * could not read its detection directory -- keep the previous list
   * and surface inside the dialog, carrying the daemon's own diagnosis.
   */
  async function loadDetections(): Promise<void> {
    try {
      const result = await invokeListDetections();
      if (result.warning !== null) {
        detectionListWarning = describeBoundaryNotice({
          operation: 'detectionsRefresh',
          detail: result.warning,
        });
        return;
      }
      detectionListWarning = null;
      detectionNames = result.names;
    } catch (error) {
      logBoundaryFailure('detectionsList', error);
      detectionListWarning = describeBoundaryNotice({ operation: 'detectionsList', detail: error });
    }
  }

  /**
   * Refreshes the launch profiles the dropdown offers: once after startup and
   * again whenever the dropdown takes focus, so a profile file added while
   * the application runs shows up without a restart.
   *
   * Both ways of failing -- a reply that fails validation and a daemon that
   * reported why it had to exclude something -- surface inside the dialog,
   * the same shape `loadDetections` has. A warning does not discard what did
   * come back: the daemon reports one line per excluded file, so a single bad
   * file must not cost the user the rest of the list. An empty list degrades
   * the field to Custom on its own.
   *
   * The list is taken as the daemon sent it, unavailable entries included:
   * the dialog is what shows them as disabled with their reason. It is
   * ordered by display name here, because that is the order the field offers
   * them in, and the dialog renders the list it is handed.
   */
  async function loadProfiles(): Promise<void> {
    try {
      const result = await invokeListProfiles();
      if (result.warnings.length > 0) {
        profileListWarning = describeBoundaryNotice({
          operation: 'profilesIncomplete',
          detail: result.warnings,
        });
      } else {
        profileListWarning = null;
      }
      profileOptions = result.profiles
        .slice()
        .sort((left, right) => left.displayName.localeCompare(right.displayName));
    } catch (error) {
      logBoundaryFailure('profilesList', error);
      profileListWarning = describeBoundaryNotice({ operation: 'profilesList', detail: error });
    }
  }

  /**
   * The select carries theme names as its option values. A user theme can
   * never take a built-in name, so the name alone tells the two selection
   * forms apart.
   */
  function toAppThemeSelection(value: string): AppThemeSelection {
    return isBuiltInSelection(value) ? value : { named: value };
  }

  function fromAppThemeSelection(selection: AppThemeSelection): string {
    return typeof selection === 'string' ? selection : selection.named;
  }

  function handleAppThemeChange(event: Event): void {
    const target = event.currentTarget;
    if (!(target instanceof HTMLSelectElement)) {
      return;
    }
    themeStore.select(toAppThemeSelection(target.value));
  }

  /**
   * Surfaces a boundary validation failure application-wide. It cannot be
   * shown on the window it came from: the payload failed validation, so its
   * `window_id` is not trustworthy enough to attribute the failure to a pane.
   */
  function reportBoundaryNotice(operation: BoundaryOperation, detail: unknown): void {
    logBoundaryFailure(operation, detail);
    boundaryNotice = describeBoundaryNotice({ operation, detail });
  }

  /**
   * The one place that logs a boundary failure to the console.
   */
  function logBoundaryFailure(operation: BoundaryOperation, detail: unknown): void {
    console.error(`${operation} boundary error:`, detail);
  }

  /**
   * Surfaces a refused window operation in the application-wide banner, the
   * same place the daemon warnings go. Operations started here are not put to
   * a human approver -- this GUI is the approver -- so a refusal is a real
   * failure, including the one the core reports on its own when approver
   * registration has not succeeded yet (`ApproverNotRegistered`).
   */
  function reportOperationFailure(summary: string, error: unknown): void {
    console.error(summary, error);
    operationError = describeOperationFailure(summary, error);
  }

  async function refreshWindowList(): Promise<void> {
    try {
      await applyWindowList(await invokeListWindows());
    } catch (error) {
      console.error('failed to list windows:', error);
    }
  }

  /** Bridges the registry's mutators to the shape `applyLayoutSteps` drives. */
  function buildLayoutTarget(): LayoutTarget {
    return {
      setWindowPosition: registry.setWindowPosition,
      setWindowSize: registry.setWindowSize,
      setWindowMinimized: registry.setWindowMinimized,
      bringWindowToFront: registry.bringWindowToFront,
      setWindowMaximized: registry.setWindowMaximized,
      moveTab: registry.moveTab,
    };
  }

  /**
   * Fetches the restore candidates once at startup. A
   * failure leaves the candidate list empty and surfaces
   * `banners.restoreLoadFailed`; it is not part of `banner-model.ts`'s fixed
   * banner set, so it renders through its own element.
   */
  async function loadRestoreCandidates(): Promise<void> {
    try {
      const result = await invokeListRestoreCandidates();
      restoreCandidates = result.candidates;
      restoreTakenAtEpochS = result.takenAtEpochS;
    } catch (error) {
      console.error('failed to list restore candidates:', error);
      restoreLoadFailedNotice = banners.restoreLoadFailed;
    }
  }

  /**
   * Recomputes the default selection against the window set as it is right
   * now -- a window opened since the candidates were fetched must
   * already show as "already open" the first time the dialog appears -- and
   * then re-applies the per-candidate overrides, so an untouched candidate
   * follows the fresh default while a touched one keeps its own value.
   */
  function openRestoreDialog(): void {
    restoreSelection = applySelectionOverrides(
      defaultSelection(restoreCandidates, restoreOpenWindows),
      restoreSelectionOverrides
    );
    isRestoreDialogOpen = true;
  }

  function closeRestoreDialog(): void {
    isRestoreDialogOpen = false;
  }

  function dismissRestoreBanner(): void {
    isRestoreBannerDismissed = true;
  }

  function toggleRestoreSelection(position: number): void {
    const isChecked = !restoreSelection.has(position);
    restoreSelectionOverrides.set(position, isChecked);
    restoreSelection = applySelectionOverrides(restoreSelection, new Map([[position, isChecked]]));
  }

  function selectAllRestoreCandidates(): void {
    for (const candidate of restoreCandidates) {
      restoreSelectionOverrides.set(candidate.position, true);
    }
    restoreSelection = new Set(restoreCandidates.map((candidate) => candidate.position));
  }

  function clearAllRestoreCandidates(): void {
    for (const candidate of restoreCandidates) {
      restoreSelectionOverrides.set(candidate.position, false);
    }
    restoreSelection = new Set();
  }

  /**
   * Opens every checked candidate (position order) and applies its layout.
   * A candidate that fails to open is skipped, so
   * one refusal does not abandon the rest.
   */
  /**
   * Opens one restore launch: a profile carries no argv of its own, so it is
   * expanded server-side, the same way {@link handleOpenWindow}'s `'profile'`
   * branch does; a saved argv carries its own detection definition and
   * vendor badge, so those come back the way the window went instead of
   * falling back to generic detection.
   */
  async function invokeRestoreLaunch(launch: RestoreLaunch): Promise<OpenWindowResult> {
    if (launch.kind === 'profile') {
      return invokeOpenWindow({
        command: [],
        cwd: launch.cwd,
        name: launch.name,
        profile: launch.profile,
        model: launch.model ?? undefined,
        effort: launch.effort ?? undefined,
      });
    }
    return invokeOpenWindow({
      command: launch.command,
      cwd: launch.cwd,
      name: launch.name,
      detection: launch.detection ?? undefined,
      vendor: launch.vendor ?? undefined,
    });
  }

  async function handleRestoreSelected(): Promise<void> {
    const selected = restoreCandidates
      .filter((candidate) => restoreSelection.has(candidate.position))
      .sort((a, b) => a.position - b.position);
    isRestoreDialogOpen = false;
    isRestoreBannerDismissed = true;

    const targets: LayoutApplicationTarget[] = [];
    // Names of the windows a disabled profile held back, collected across
    // the whole restore and reported once below, not per window.
    const disabledProfileNames: string[] = [];
    for (const candidate of selected) {
      try {
        let opened: OpenWindowResult;
        try {
          opened = await invokeRestoreLaunch(restoreLaunchOf(candidate));
        } catch (error) {
          const failure = windowOpenFailureOf(error);
          // A profile the user switched off is not retried under its saved
          // argv: that would start the very CLI they turned off.
          if (isProfileDisabledRefusal(failure)) {
            disabledProfileNames.push(candidate.name);
            continue;
          }
          // A profile the daemon no longer recognizes still has the argv it
          // was recorded with to fall back to; every other failure kind is
          // left to propagate (restoreFallbackOf).
          const fallback = restoreFallbackOf(candidate, failure);
          if (fallback === null) {
            throw error;
          }
          opened = await invokeRestoreLaunch(fallback);
        }
        targets.push({
          windowId: opened.windowId,
          position: candidate.position,
          layout: candidate.layout,
        });
      } catch (error) {
        console.error('failed to restore window:', error);
      }
    }
    await refreshWindowList();
    applyLayoutSteps(buildLayoutTarget(), planLayoutApplication(targets, get(registry).tabOrder));
    const disabledNotice = restoreNoticeForDisabled(disabledProfileNames);
    if (disabledNotice !== null) {
      operationError = disabledNotice;
    }
  }

  /**
   * Brings the panes in line with one window-list snapshot.
   *
   * Exited windows are left out: the registry retires a window as soon as it
   * reports `exited`, while the daemon keeps the entry in its list until it is
   * evicted, so keeping them would re-open panes the view has already dropped.
   */
  async function applyWindowList(windows: WindowInfo[]): Promise<void> {
    const liveWindows = windows.filter((info) => info.detected.status !== 'exited');
    for (const info of liveWindows) {
      registry.openWindow(info);
    }
    const livePaneIds = [...get(panes).keys()];
    const plan = await syncWindows(
      liveWindows.map((info) => info.window_id),
      livePaneIds,
      {
        openPane,
        closePane,
        onError: (windowId, error) => {
          console.error(`failed to open pane for ${windowId}:`, error);
        },
      }
    );
    // Only the panes this snapshot just opened get their daemon-held layout
    // back; a live one keeps whatever the user is doing with it.
    const targets = layoutTargetsForOpened(liveWindows, plan.toOpen);
    if (targets.length > 0) {
      applyLayoutSteps(
        buildLayoutTarget(),
        layoutStepsForSnapshot(targets, get(registry).tabOrder, livePaneIds.length === 0)
      );
    }
    placePendingWindows();
    // Participant counts are derived from the window registry, so opening or
    // closing a window moves the room list with no chat push behind it; only
    // the open panel shows those counts.
    if (isChatOpen) {
      void refreshChatRooms();
    }
    // Forgets the collapsed state of any room that no longer holds a window.
    collapsedRooms.prune(roomIdsOf(get(registry).windows));
  }

  /**
   * Keeps `area` on the measured size of the float area: the layout functions
   * take the area as an argument and never measure the DOM themselves.
   */
  function trackFloatArea(element: HTMLElement): { destroy: () => void } {
    const measure = (): void => {
      applyArea({ width: element.clientWidth, height: element.clientHeight });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return {
      destroy: () => {
        observer.disconnect();
      },
    };
  }

  function applyArea(next: Size): void {
    if (next.width === area.width && next.height === area.height) {
      return;
    }
    // Only the area changes: each float window fits the rect the user placed
    // into it as it renders, so a smaller area never overwrites that rect.
    area = next;
    placePendingWindows();
  }

  /**
   * A zero-sized area means the float area has not been laid out, not that it
   * is tiny: clamping to it would collapse every window onto the origin.
   */
  function isMeasurable(size: Size): boolean {
    return size.width > 0 && size.height > 0;
  }

  /**
   * Gives every window that has never been placed a cascade rectangle, one at
   * a time: `cascadeRect` (`layout/float-layout.ts`) offsets by the number of
   * windows already placed, so that count is re-read for each of them and
   * restoring a session does not stack all of its windows on one spot.
   */
  function placePendingWindows(): void {
    if (!isMeasurable(area)) {
      return;
    }
    const pending = get(registry)
      .windows.filter((entry) => entry.display.position === null)
      .map((entry) => entry.windowId);
    for (const windowId of pending) {
      const placed = get(registry).windows.filter(
        (entry) => entry.display.position !== null
      ).length;
      const rect = cascadeRect(area, placed);
      registry.setWindowPosition(windowId, rect.position);
      registry.setWindowSize(windowId, rect.size);
    }
  }

  /**
   * Measures the main content area for tiling: the region the
   * windows end up in still includes the strip the tab bar occupies right
   * now. Shared by `handleArrangeWindows` and the per-room tile
   * action, so the measurement is not taken twice.
   */
  function measureTileArea(): Size | null {
    if (mainContentElement === undefined) {
      return null;
    }
    const rect = mainContentElement.getBoundingClientRect();
    return { width: rect.width, height: rect.height };
  }

  /** Tiles every open window over the main content area. */
  function handleArrangeWindows(): void {
    const area = measureTileArea();
    if (area === null) {
      return;
    }
    registry.arrangeWindows(area);
  }

  /**
   * Creates the pane of one window. The pane runs the attach handshake from
   * its own `onMount`, so waiting for the render is what keeps the windows
   * attaching in the daemon's list order.
   */
  async function openPane(windowId: string): Promise<void> {
    if (get(panes).has(windowId)) {
      return;
    }
    const sessionStore = createWindowSessionStore();
    const controller = new SessionController(windowId, sessionStore);
    // eslint-disable-next-line svelte/prefer-svelte-reactivity -- store snapshot swap: a fresh Map is republished through the writable, so runes-based SvelteMap reactivity does not apply
    panes.update((current) => new Map(current).set(windowId, { controller, sessionStore }));
    registry.registerWindowHandlers(windowId, controller.eventHandlers());
    await tick();
  }

  function closePane(windowId: string): void {
    const pane = get(panes).get(windowId);
    if (pane === undefined) {
      return;
    }
    // Computed against the tab order from *before* the removal:
    // nextActiveTab locates the neighbours of `windowId`,
    // which only works while it is still in the order. `wasActiveTab` is
    // read the same way, ahead of the registry update below that drops it.
    const state = get(registry);
    const wasActiveTab =
      state.windows.find((entry) => entry.windowId === windowId)?.display.isMaximized ?? false;
    const successor = nextActiveTab(state.tabOrder, isEligibleTabSuccessor, windowId);
    // The window leaves the render source (renderedPanes) before its pane is
    // torn down, so no flush can observe a windowId with no pane behind it.
    registry.closeWindow(windowId);
    pane.controller.dispose();
    panes.update((current) => {
      // eslint-disable-next-line svelte/prefer-svelte-reactivity -- store snapshot swap: a fresh Map is republished through the writable, so runes-based SvelteMap reactivity does not apply
      const next = new Map(current);
      next.delete(windowId);
      return next;
    });
    // A non-active tab closing only removes its tab: the strip stays
    // exactly as it was for every other tab, so this only acts when the
    // window that just closed was the one the strip was showing.
    if (wasActiveTab) {
      if (successor === null) {
        registry.setWindowMaximized(windowId, false);
      } else {
        switchToTab(successor);
      }
    }
  }

  /**
   * Opens the window the dialog submitted. A refusal is left to propagate:
   * the dialog is where the rejected input still is, so that is where it is
   * shown, and the dialog stays open on it.
   */
  async function handleOpenWindow(input: OpenWindowSubmission): Promise<void> {
    detectionWarning = null;
    // A blank detection definition is left out of the call: the core tells
    // "not provided" apart from an empty value, and validates both. One call
    // per branch: a profile carries its own command, detection definition and
    // vendor, and the core refuses any of those sent beside it.
    let opened: OpenWindowResult;
    try {
      if (input.kind === 'custom') {
        opened = await invokeOpenWindow({
          command: input.tokens,
          cwd: input.cwd,
          name: input.name,
          detection: detectionInput.trim() || undefined,
        });
      } else if (input.kind === 'profile') {
        opened = await invokeOpenWindow({
          command: [],
          cwd: input.cwd,
          name: input.name,
          profile: input.profile,
          model: input.model,
          effort: input.effort,
        });
      } else {
        // Fail closed: a submission kind this function does not know opens
        // nothing rather than an approximation of it.
        return;
      }
    } catch (failed) {
      // A refusal can make the remembered directory not worth offering again.
      // The refusal itself still reaches the dialog, which stays open on it.
      if (shouldDropRememberedCwd(windowOpenFailureOf(failed))) {
        profileMemory.dropCwd();
      }
      throw failed;
    }
    // Received under another name: the component state of the same name is
    // what the warning is written to just below.
    const { windowId, detectionWarning: warning } = opened;
    // The window is open either way; a warning only says its status will
    // stay `Unknown` because the definition could not be loaded.
    if (warning !== null) {
      detectionWarning = describeDetectionWarning(warning);
    }
    // The pane is created from the window list, the same path the core's
    // poll takes, so opening twice cannot produce two panes for one window.
    await refreshWindowList();
    // A window opened while in tab mode joins the strip and is switched to;
    // otherwise it is raised. Either way the keyboard follows it.
    if (isTabMode()) {
      switchToTab(windowId);
    } else {
      registry.bringWindowToFront(windowId);
    }
    focusTerminalOf(windowId);
    // What opened is what the dialog offers next time; a profile launch also
    // remembers the model/effort it used, keyed by that profile id, so
    // reopening the same profile offers them again. A Custom launch does not
    // remember a profile id (there is none to key the launched options by),
    // but keeps whatever was already remembered for other profiles.
    const remembered = profileMemory.read();
    const next =
      input.kind === 'profile'
        ? withRememberedOption(
            { profileId: input.profile, cwd: input.cwd ?? null, options: remembered.options },
            input.profile,
            { model: input.model ?? null, effort: input.effort ?? null }
          )
        : { profileId: null, cwd: input.cwd ?? null, options: remembered.options };
    profileMemory.write(next);
    detectionInput = '';
    cwdInput = '';
    profileInput = '';
    modelInput = '';
    effortInput = '';
    isAddDialogOpen = false;
  }

  /**
   * Opens the dialog on what was launched last, with the working directory
   * that profile declares, else the one used last, else empty. A remembered
   * profile the list no longer offers falls back to the first entry, and an
   * empty list to the Custom form.
   */
  function openAddDialog(): void {
    const remembered = profileMemory.read();
    const offered =
      offeredProfileOptions.find((entry) => entry.id === remembered.profileId) ??
      offeredProfileOptions[0];
    profileInput = offered?.id ?? '';
    cwdInput = offered?.cwd ?? remembered.cwd ?? '';
    // No offered profile means nothing is loaded into the launch-option
    // fields either -- there is no id to look their memory up by.
    const record = offered !== undefined ? remembered.options[offered.id] : undefined;
    modelInput = record?.model ?? '';
    effortInput = record?.effort ?? '';
    isAddDialogOpen = true;
  }

  /**
   * Every dismissal reaches here, and dismissing discards what was typed,
   * including the fields held here on the dialog's behalf.
   * A refused submission never gets here: it leaves the dialog open on the
   * input so it can be corrected.
   */
  function closeAddDialog(): void {
    detectionInput = '';
    cwdInput = '';
    profileInput = '';
    modelInput = '';
    effortInput = '';
    isAddDialogOpen = false;
  }

  function handleDetectionChange(value: string): void {
    detectionInput = value;
  }

  function handleProfileChange(value: string): void {
    profileInput = value;
    // Switching to a different profile (or to Custom) loads that profile's
    // own remembered launch options, never the previous selection's.
    const record = profileMemory.read().options[value];
    modelInput = record?.model ?? '';
    effortInput = record?.effort ?? '';
  }

  function handleModelChange(value: string): void {
    modelInput = value;
  }

  function handleEffortChange(value: string): void {
    effortInput = value;
  }

  function handleCwdChange(value: string): void {
    cwdInput = value;
  }

  async function handleBrowse(): Promise<void> {
    try {
      const path = await pickFolder();
      if (path !== null) {
        cwdInput = path;
      }
    } catch (error) {
      // The native picker could not be opened. A refusal and a cancel look
      // the same from here, so the field keeps what the user typed.
      console.error('failed to open folder picker:', error);
    }
  }

  function handleRenameWindow(entry: WindowEntry): void {
    renameDialog = { isOpen: true, entry };
  }

  function handleDismissBanner(id: BannerId): void {
    const next = dismissBanner(bannerState, id);
    daemonSpawnNotice = next.daemonSpawnNotice;
    operationError = next.operationError;
    detectionWarning = next.detectionWarning;
    keymapNotice = next.keymapNotice;
    boundaryNotice = next.boundaryNotice;
    isThemeLoadFailureDismissed = next.isThemeLoadFailureDismissed;
    isThemeIssuesDismissed = next.isThemeIssuesDismissed;
    approvalExpiredUnseen = next.approvalExpiredUnseen;
    isDaemonRecoveryDismissed = next.isDaemonRecoveryDismissed;
    clientConfigFailure = next.clientConfigFailure;
    startupEnvFailure = next.startupEnvFailure;
    isProfileUnavailableDismissed = next.isProfileUnavailableDismissed;
  }

  function handleBannerAction(action: BannerAction): void {
    switch (action.labelKey) {
      case 'reconnect':
        void handleReconnect();
        return;
      case 'howToInstall':
        void showProfileInSettings(action.profileId);
        return;
    }
  }

  /** Opens Settings on the Profiles tab with `id`'s row open, where its guidance is shown. */
  async function showProfileInSettings(id: string): Promise<void> {
    if (!isSettingsOpen) {
      await toggleSettings();
    }
    selectSettingsTab('profiles');
    if (openProfileDetailId !== id) {
      toggleProfileDetail(id);
    }
  }

  /**
   * Asks the supervisor to start the daemon again (exhausted-only action).
   *
   * The command never reports a failed start: whether the daemon came back is
   * answered by the next recovery status, which moves the banner within a tick.
   * The catch below guards the bridge itself, not the start.
   */
  async function handleReconnect(): Promise<void> {
    try {
      await invokeReconnectDaemon();
    } catch (error) {
      console.error('failed to trigger a manual daemon reconnect:', error);
    }
  }

  // Initialize on mount. The two enumerations are chained behind the startup
  // sequence so they do not compete with it for the one control connection.
  void initialize().then(() => Promise.all([loadDetections(), loadProfiles()]));
  void loadKeymap();
  void loadKeymapFilePath();
  void loadThemes();
  void loadRestoreCandidates();
</script>

<svelte:window on:keydown|capture={handleWindowKeydown} on:resize={handleWindowResize} />
<!-- `close` does not bubble, so it is caught on the way down. -->
<svelte:document on:close|capture={() => pendingFocus.flush()} on:focusin={notePanelFocus} />

<main class="app">
  {#if restoreLoadFailedNotice !== null}
    <div class="restore-load-failed" role="alert">{restoreLoadFailedNotice}</div>
  {/if}

  {#each daemonHealthNotices as notice (notice)}
    <div class="restore-load-failed" role="alert">{notice}</div>
  {/each}

  {#if restoreCandidates.length > 0 && !isRestoreBannerDismissed}
    <RestoreBanner
      count={restoreCandidates.length}
      timestamp={restoreTakenAtEpochS === null
        ? null
        : formatRestoreTimestamp(restoreTakenAtEpochS)}
      onChoose={openRestoreDialog}
      onDismiss={dismissRestoreBanner}
    />
  {/if}

  {#each activeBanners as banner (banner.id)}
    <div class="banner banner-warning" role="alert">
      <span class="banner-text">{banner.text}</span>
      {#if banner.action}
        {@const action = banner.action}
        <button class="banner-action" type="button" on:click={() => handleBannerAction(action)}>
          {buttons[action.labelKey]}
        </button>
      {/if}
      <IconButton
        class="banner-close"
        label={hints.dismissBanner}
        on:click={() => handleDismissBanner(banner.id)}
      >
        ✕
      </IconButton>
    </div>
  {/each}

  <RestoreDialog
    isOpen={isRestoreDialogOpen}
    onCancel={closeRestoreDialog}
    candidates={restoreCandidates}
    openWindows={restoreOpenWindows}
    selected={restoreSelection}
    onToggle={toggleRestoreSelection}
    onSelectAll={selectAllRestoreCandidates}
    onClearAll={clearAllRestoreCandidates}
    onRestore={handleRestoreSelected}
  />

  <div class="layout">
    <AppRail
      attentionCount={attentionCount($registry.windows)}
      {appThemeValue}
      {appThemeOptions}
      onAppThemeChange={handleAppThemeChange}
      {chatNeedsUser}
      onChatToggle={toggleChat}
      onPlansToggle={togglePlans}
      {isPlansOpen}
      {isChatOpen}
      plansAttention={plansAttention(planList)}
      onSettingsToggle={toggleSettings}
      {isSettingsOpen}
    />

    <aside class="sidebar" bind:this={sidebarElement} style="flex-basis: {sidebarWidth}px;">
      <StatusPanel
        entries={$registry.windows}
        {actions}
        {roomActions}
        collapsed={$collapsedRooms}
        tabOrder={$registry.tabOrder}
        onGoToNextNeedingYou={attentionNavigation.goToNextWindowNeedingYou}
      >
        <div slot="header-action" class="header-actions">
          <IconButton
            class="btn btn-primary add-window-trigger"
            label={forms.addWindowTitle}
            on:click={openAddDialog}
          >
            +
          </IconButton>
          <IconButton
            class="status-panel__button arrange-window-trigger"
            label={hints.tileWindows}
            on:click={handleArrangeWindows}
          >
            <TileIcon />
          </IconButton>
        </div>
      </StatusPanel>
    </aside>

    <!-- Pointer-only affordance: keyboard resize belongs to the shortcut
         layer, so the divider is not exposed to assistive technology. -->
    <div class="sidebar-resize" aria-hidden="true" use:pointerDrag={sidebarDrag}></div>

    <section class="main-content" bind:this={mainContentElement}>
      {#if activeTabEntry !== undefined}
        <TabBar
          entries={tabEntries}
          activeEntry={activeTabEntry}
          {actions}
          {themeStore}
          onRename={handleTabRename}
          onSelectTab={switchToTab}
          onReorderTab={registry.moveTab}
        />
      {/if}
      <div class="float-area" use:trackFloatArea>
        {#if $registry.windows.length === 0}
          <div class="empty-main">
            <p>{hints.noWindows}</p>
          </div>
        {/if}
        {#each renderedPanes as rendered (rendered.entry.windowId)}
          <FloatWindow
            entry={rendered.entry}
            {area}
            {layout}
            {actions}
            {themeStore}
            controller={rendered.pane.controller}
            sessionStore={rendered.pane.sessionStore}
            isVisible={isWindowVisible(rendered.entry, activeTabEntry)}
            isCurrent={rendered.entry.windowId === current?.windowId}
            onRename={() => handleRenameWindow(rendered.entry)}
          />
        {/each}
      </div>
    </section>
  </div>
</main>

{#if isChatOpen}
  <ChatPanel
    room={chatRoomStates.get(currentRoomId) ?? emptyChatRoom(currentRoomId)}
    rooms={[...chatRooms.values()]}
    {currentRoomId}
    onSelectRoom={selectChatRoom}
    onClose={closeChat}
    onSend={(body: string) => postChat(body, false)}
    onBroadcast={broadcastChat}
  ></ChatPanel>
{/if}

{#if isPlansOpen}
  <PlansPanel
    {planList}
    {selectedPlanId}
    plan={displayedPlan}
    {roles}
    {defaultRole}
    windows={$registry.windows}
    onClose={closePlans}
    onSelectPlan={selectPlan}
    onDeletePlan={(planId: string) => void deletePlan(planId)}
    onSetTaskAssignment={(taskId: string, assignment: TaskAssignment) =>
      void setTaskAssignment(taskId, assignment)}
    onClearTaskAssignment={(taskId: string) => void clearTaskAssignment(taskId)}
    onSetPlanOwner={setPlanOwner}
    onSaveRoles={(next: RoleEntry[], nextDefaultRole: string | null) =>
      void saveRoles(next, nextDefaultRole)}
    onApprovePlan={() => void approvePlan()}
    onBoardTabSelected={notePlanViewed}
    profileOptions={offeredProfileOptions}
    {shippedModels}
    {requestedModelProfile}
    {modelChoices}
    {isModelAliasesReadOnly}
    onSelectModelProfile={selectModelProfile}
    onRefreshModelChoices={refreshModelChoices}
    onSaveModelAliases={(profile: string, aliases: LaunchOptionAlias[]) =>
      void saveModelAliases(profile, aliases)}
    size={plansPanelSize}
    maxSize={panelMax}
    onResize={handlePlansPanelResize}
  />
{/if}

{#if isSettingsOpen}
  <SettingsPanel
    activeTab={settingsTab}
    onSelectTab={selectSettingsTab}
    onClose={closeSettings}
    {profileOptions}
    {roles}
    isReadOnly={isSettingsReadOnly}
    {savingProfileId}
    updateFailure={profileUpdateFailure}
    onSetProfileEnabled={handleSetProfileEnabled}
    {profileDetails}
    {profileDetailActions}
    {approvalSettings}
    {approvalNotice}
    {isApprovalReadOnly}
    {isApprovalSaving}
    approvalUpdateFailure={approvalSwitchFailure}
    onSetAutoApprove={handleSetAutoApprove}
    onSetPlanApprovalRequired={handleSetPlanApprovalRequired}
    {integrationsListed}
    {integrationStatuses}
    {integrationRunning}
    {integrationResults}
    integrationsNotice={integrationsNoticeLine}
    {integrationsFailure}
    onRunMcpRegistration={runMcpRegistration}
    keys={$keymapStore.keys}
    platform={keyboardPlatform}
    loadStatus={keymapLoadStatus}
    saveFailure={$keymapStore.saveFailure}
    keyFilePath={keymapFilePath}
    onBeginRecording={shortcutDispatcher.beginRecording}
    onCancelRecording={shortcutDispatcher.cancelRecording}
    onCommit={keymapStore.commit}
    size={settingsPanelSize}
    maxSize={panelMax}
    onResize={handleSettingsPanelResize}
  />
{/if}

<AddWindowDialog
  bind:isOpen={isAddDialogOpen}
  onCancel={closeAddDialog}
  warning={detectionListWarning}
  cwdValue={cwdInput}
  onCwdChange={handleCwdChange}
  onBrowse={handleBrowse}
  {isDropTarget}
  detectionValue={detectionInput}
  onDetectionChange={handleDetectionChange}
  {detectionOptions}
  onLoadDetections={() => void loadDetections()}
  profileOptions={offeredProfileOptions}
  profileValue={profileInput}
  onProfileChange={handleProfileChange}
  onLoadProfiles={() => void loadProfiles()}
  profileWarning={profileListWarning}
  modelValue={modelInput}
  onModelChange={handleModelChange}
  effortValue={effortInput}
  onEffortChange={handleEffortChange}
  onSubmit={handleOpenWindow}
/>

<ConfirmDialog
  bind:isOpen={confirmDialog.isOpen}
  title={confirmWording.title}
  message={confirmWording.message}
  confirmLabel={confirmWording.confirmLabel}
  operation={confirmDialog.operation}
  onConfirm={() => answerConfirm(true)}
  onCancel={() => answerConfirm(false)}
/>

{#if approvalRequest !== null}
  <ApprovalDialog
    isOpen={isApprovalDialogOpen}
    request={approvalRequest}
    isRequesterOpen={isApprovalRequesterOpen}
    pendingCount={approvalQueue.size(approvals)}
    openedAt={approvalOpenedAt}
    onDecide={decideApproval}
  />
{/if}

<RenameDialog
  bind:isOpen={renameDialog.isOpen}
  title={dialogs.renameTitle}
  label={forms.windowNameLabel}
  initialValue={renameDialog.entry?.name ?? ''}
  onConfirm={(name: string) => {
    const entry = renameDialog.entry;
    if (entry !== null) {
      void actions.rename(entry, name);
    }
  }}
  onCancel={() => {
    // Dialog will close automatically
  }}
/>

<style>
  :global(html),
  :global(body) {
    height: 100%;
    margin: 0;
    background-color: var(--color-background);
    color: var(--color-text-primary);
    font-family: var(--font-family-base);
    font-size: var(--font-size-base);
    line-height: var(--line-height-base);
  }

  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }

  .banner {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    border-bottom: 1px solid var(--color-border);
    font-size: var(--font-size-sm);
  }

  .banner-warning {
    background-color: var(--color-warning);
    color: var(--color-on-warning);
    border-bottom-color: var(--color-on-warning);
  }

  /* The restore candidate fetch's failure notice. Not part of the
     .banner/.banner-warning skeleton above -- RestoreBanner is deliberately
     not integrated into banner-model.ts, so this stays a
     distinct element rather than a second literal use of that class pair. */
  .restore-load-failed {
    padding: var(--space-sm) var(--space-md);
    border-bottom: 1px solid var(--color-border);
    background-color: var(--color-warning);
    color: var(--color-on-warning);
    font-size: var(--font-size-sm);
  }

  .banner-text {
    flex: 1 1 auto;
  }

  .banner-action {
    flex: 0 0 auto;
  }

  .layout {
    display: flex;
    flex: 1 1 auto;
    min-height: 0;
    gap: 0;
  }

  /* The width comes from the element's own flex-basis, written from the value
     ./layout/sidebar-width keeps; the rows reach both edges. */
  .sidebar {
    flex-grow: 0;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: 0;
    padding: 0;
    overflow-y: auto;
    background-color: var(--color-surface);
  }

  /* Both the divider and the grab target for the resize, which is why it is
     wider than the hairline it draws. */
  .sidebar-resize {
    flex: 0 0 5px;
    border-left: 1px solid var(--color-border);
    background-color: var(--color-surface);
    cursor: ew-resize;
    touch-action: none;
  }

  /* Tighter than the header row's own spacing, so the two buttons read as one
     group rather than as two unrelated header controls. */
  .header-actions {
    display: flex;
    gap: var(--space-xs);
  }

  :global(.btn.add-window-trigger),
  :global(.status-panel__button.arrange-window-trigger) {
    flex: 0 0 auto;
    width: 28px;
    height: 28px;
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: var(--font-size-md);
    line-height: 1;
  }

  /* Holds the optional tab strip above the float area, stacked;
     the containing block for the windows lives on .float-area now. */
  .main-content {
    display: flex;
    flex-direction: column;
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
  }

  /* Windows are absolutely positioned against this element and their
     rectangles come from its measured size, so it establishes the containing
     block and never scrolls. */
  .float-area {
    position: relative;
    flex: 1 1 auto;
    min-height: 0;
    overflow: hidden;
  }

  .empty-main {
    display: flex;
    height: 100%;
    align-items: center;
    justify-content: center;
    color: var(--color-text-secondary);
  }
</style>
