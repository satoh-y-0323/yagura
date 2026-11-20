<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import { get, writable } from 'svelte/store';

  import { describeBoundaryNotice, type BoundaryOperation } from '../notices/boundary-notice';
  import type { OpenWindowSubmission, ProfileOption } from '../components/add-window-dialog-types';
  import AddWindowDialog from '../components/AddWindowDialog.svelte';
  import AlertStrip from '../components/AlertStrip.svelte';
  import AppRail from '../components/AppRail.svelte';
  import ApprovalDialog from '../components/ApprovalDialog.svelte';
  import ChatPanel from '../components/ChatPanel.svelte';
  import CommandPicker from '../components/CommandPicker.svelte';
  import ConfirmDialog from '../components/ConfirmDialog.svelte';
  import NoticeAnnouncer from '../components/NoticeAnnouncer.svelte';
  import NoticeCenter from '../components/NoticeCenter.svelte';
  import PlansPanel from '../components/PlansPanel.svelte';
  import RenameDialog from '../components/RenameDialog.svelte';
  import RestoreDialog from '../components/RestoreDialog.svelte';
  import SettingsPanel from '../components/SettingsPanel.svelte';
  import ToastRegion from '../components/ToastRegion.svelte';
  import Tooltip from '../components/Tooltip.svelte';
  import type { OutsideDismissReason } from '../components/dismiss-on-outside';
  import type { Rect } from '../components/menu-position';
  import { focusAsLastInput } from '../components/input-modality';
  import { bodyEscapePanel } from '../components/panel-focus';
  import { pointerDrag, type PointerDragOptions } from '../components/pointer-drag';
  import {
    anyNeedsUser,
    applyChatChangedToRooms,
    applyChatHistoryCleared,
    applyRoomsList,
    chatReloadAfterSeq,
    chatRoomGeneration,
    emptyChatRoom,
    foldChatPage,
    roomAfterRemoval,
    type ChatHistory,
    type ChatRoomState,
  } from '../chat/chat-store';
  import { describeRoomRemovalFailure, readRefusedForRemovedRoom } from '../chat/room-actions';
  import { serializeRefresh } from '../chat/refresh-serializer';
  import { daemonProblemNotices } from '../notices/daemon-health';
  import {
    appendEvent,
    daemonReconnectedEvent,
    dismissEvent,
    failureEvent,
    windowExitEvent,
    type EventLog,
    type FailureEventKey,
    type NewEvent,
  } from '../notices/event-log';
  import {
    emptyNeedsYouToastState,
    stepNeedsYouToasts,
    type NeedsYouToastState,
  } from '../notices/needs-you-toasts';
  import {
    dismissLaneNotice,
    emptyLaneState,
    emptyNoticeMemory,
    noticeFeed,
    type LaneItem,
    type Notice,
    type NoticeFeedInput,
    type NoticeFeedOutput,
  } from '../notices/notice-feed';
  import {
    applyClientConfigFailure,
    applyRecoveryUpdate,
    buildNotices,
    dismissNotice,
    profilesNeedingNativeProgram,
    recordApprovalExpiredUnseen,
    reopenOffer,
    type DaemonRecoveryUpdate,
    type NoticeAction,
    type NoticeCategory,
    type NoticeSourceState,
  } from '../notices/notice-model';
  import type { DropSignal } from '../ipc/folder-drop-model';
  import { writeClipboardText } from '../ipc/clipboard';
  import { pickFolder, subscribeFolderDrop } from '../ipc/folder-picker';
  import { startFramePull, type FramePull } from '../ipc/frame-pull';
  import { sanitizeForDisplay } from '../display/untrusted-text';
  import {
    ATTACH_VIEW_MIN_REVISION,
    type ApprovalRequest,
    type ChatChanged,
    type ChatHistoryCleared,
    type ChatPosted,
    type ChatRoomInfo,
    type DaemonConnectedPayload,
    type RestoreEntry,
  } from '../ipc/session-boundary';
  import {
    invokeNextFrames,
    subscribeChatChanged,
    subscribeChatHistoryCleared,
    subscribeChatParticipantsChanged,
    subscribeSession,
    subscribeWindowList,
    invokeBroadcastChat,
    invokeClearChatRoom,
    invokeDeleteChatRoom,
    invokeGetApprovalSettings,
    invokeListChatRooms,
    invokeListRestoreCandidates,
    invokeOpenWindow,
    invokeCloseWindow,
    invokeDecideApproval,
    invokeGetRecoveryStatus,
    invokeGetDaemonConnection,
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
    invokeSetDefaultPalettes,
    invokeReadChat,
    invokeReconnectDaemon,
    invokeRenameWindow,
    invokeSendChat,
    invokeSetAutoApprove,
    invokeSetAttentionSummary,
    invokeSetPlanApprovalRequired,
    invokeSetProfileEnabled,
    invokeUpdateWindowLayout,
    type ApprovalSettingsInfo,
    type OpenWindowResult,
    type WindowInfo,
  } from '../ipc/session-client';
  import type {
    PlanChanged,
    PlanInfo,
    PlanSummary,
    ReviewMark,
    RoleEntry,
    TaskAssignment,
  } from '../ipc/plan-boundary';
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
  } from '../ipc/plan-client';
  import type { Ticket, TicketChanged, TicketList, TicketWorkspace } from '../ipc/ticket-boundary';
  import {
    invokeGetTicket,
    invokeListTicketWorkspaces,
    invokeListTickets,
    subscribeTicketChanged,
  } from '../ipc/ticket-client';
  import {
    MODEL_ALIASES_MIN_REVISION,
    type LaunchOptionAlias,
    type ModelChoicesPayload,
    type ShippedModelProfiles,
  } from '../ipc/model-boundary';
  import {
    invokeListModelChoices,
    invokeSaveModelAliases,
    listenModelChoices,
  } from '../ipc/model-client';
  import { PROFILE_DETAIL_MIN_REVISION, type ProfileDetail } from '../ipc/profile-detail-boundary';
  import { invokeGetProfileDetail, invokeSetProfileCommand } from '../ipc/profile-detail-client';
  import { shippedAliasesOf } from '../profile/model-aliases-model';
  import {
    platformName,
    type ProfileDetailActions,
    type ProfileDetailsState,
  } from '../settings/profile-detail-model';
  import type {
    McpRegistrationAction,
    McpRegistrationResultPayload,
    McpRegistrationsListed,
    McpRegistrationStatusInfo,
    McpRegistrationStatusPayload,
  } from '../ipc/mcp-registration-boundary';
  import {
    invokeListMcpRegistrations,
    invokeRunMcpRegistration,
    listenMcpRegistrationResult,
    listenMcpRegistrationStatus,
  } from '../ipc/mcp-registration-client';
  import { hostKeyboardPlatform } from '../keymap/keyboard-platform';
  import { createKeymapStore } from '../keymap/keymap-store';
  import type { KeymapLoadStatus } from '../settings/keyboard-settings-model';
  import { createShortcutDispatcher } from '../keymap/shortcut-dispatcher';
  import { actionAvailability, type ActionVerdict } from '../keymap/action-availability';
  import type { ActionId } from '../keymap/key-binding';
  import {
    availabilityContext,
    createShortcutRunner,
    type PickerMode,
    type ShortcutRunnerDeps,
  } from '../keymap/shortcut-runner';
  import { cascadeRect, type Size } from '../layout/float-layout';
  import { clampSidebarWidth, createSidebarWidthStorage } from '../layout/sidebar-width';
  import { offeredProfiles } from '../profile/offered-profiles';
  import {
    createBrowserProfileMemoryDeps,
    createProfileMemory,
    withRememberedOption,
  } from '../profile/profile-store';
  import {
    acceptsChoicesPush,
    CHOICES_FETCH_TIMEOUT_MS,
    isChoicesFetchOverdue,
    nextChoicesRequestId,
    type ModelChoicesEntry,
  } from '../profile/model-aliases-model';
  import * as approvalQueue from '../session/approval-queue';
  import { appHidden } from '../session/app-visibility';
  import { isApprovalDialogAllowed } from '../session/modal-state';
  import { describeUntargeted } from '../session/approval-target';
  import type { WithdrawReason } from '../session/approval-types';
  import {
    plansAttention,
    planReloadFor,
    selectionAfterPlanReadFailure,
    reviewMarksOf,
    shouldMarkPlanSeen,
    withRetainedMarks,
    type PlanReload,
  } from '../plans/plan-board-model';
  import {
    buildTicketBoard,
    planBoardBecameVisible,
    ticketColumnQueries,
    ticketReloadFor,
    type PlansTab,
    type TicketBoard,
  } from '../tickets/ticket-board-model';
  import {
    approvalSettingsNotice,
    approvalUpdateFailure,
    isApprovalSettingsReadOnly,
    isApprovalSettingsSupported,
    PLAN_REVIEW_GATE_ID,
  } from '../settings/approval-settings-model';
  import {
    integrationsNotice,
    isIntegrationsSupported,
  } from '../settings/integration-settings-model';
  import {
    clampPanelSize,
    panelMaxSize,
    createPanelSizeStorage,
    isResizedByUser,
    type PanelSize as SettingsPanelSize,
  } from '../settings/panel-size';
  import { isProfileSettingsReadOnly } from '../settings/profile-settings-model';
  import { type SettingsTab } from '../settings/settings-tabs';
  import { startLayoutSync } from '../session/layout-sync';
  import { confirmCopy, type ConfirmOperation } from '../session/confirm-target';
  import { pairPanes, type Pane } from '../session/pane-plan';
  import { createAttentionNavigation } from '../session/attention-navigation';
  import { currentWindowFor, type WorkMode } from '../session/current-window';
  import {
    diffAppearedWindows,
    diffGoneWindows,
    firstWindowAtStartup,
    initialShellState,
    readWorkMode,
    reduceShell,
    workViewOf,
    writeWorkMode,
    type OrderedWindow,
    type ShellEvent,
    type ShellState,
    type ShellStep,
  } from '../session/shell-state';
  import { createGuardedLocalStorage } from '../storage/guarded-storage';
  import {
    NOTIFICATION_PREFERENCES_KEY,
    readPreferences,
    withPreference,
    writePreferences,
  } from '../notices/notification-preferences';
  import { createRoomActions } from '../session/room-actions';
  import {
    createPendingFocus,
    isInputTarget,
    lostFocusMove,
    modalReturnTarget,
    panelReturnTarget,
    sidebarToggleMove,
    windowActivationMove,
    type FocusFacts,
    type FocusMove,
    type ModalOrigin,
    type PanelOrigin,
    type PanelReturn,
  } from '../session/focus-moves';
  import { provideModalFocusPort, type ModalFocusToken } from '../session/modal-focus-port';
  import { revealWindow, type RevealDeps } from '../session/room-navigation';
  import {
    applyLayoutSteps,
    applySelectionOverrides,
    defaultSelection,
    formatRestoreTimestamp,
    planLayoutApplication,
    layoutTargetsForOpened,
    layoutStepsForSnapshot,
    restoreNoticeForFailures,
    restoreSelected,
    type LayoutApplicationTarget,
    type LayoutTarget,
    type RestoreLaunch,
  } from '../session/restore-model';
  import { SessionController } from '../session/session-controller';
  import { createCollapsedRooms } from '../sidebar/collapsed-rooms';
  import {
    nextNeedsYouView,
    visibleWindowOrder,
    type NeedsYouView,
  } from '../sidebar/needs-you-view';
  import {
    groupByRoom,
    roomIdsOf,
    roomNameOf,
    windowIdsOfRoom,
    windowsInDisplayOrder,
    type RoomGroup,
  } from '../sidebar/room-groups';
  import {
    createWindowActions,
    type ConfirmPort,
    type WindowActions,
  } from '../session/window-actions';
  import { syncWindows } from '../session/window-sync';
  import {
    attentionCount,
    beginAttentionSend,
    emptyArrivalMemory,
    emptyAttentionSendState,
    needsYou,
    noteAttentionArrival,
    observeNeedsYouArrivals,
    settleAttentionSend,
    summarizeWindowStatuses,
  } from '../status-tone';
  import { createWindowSessionStore } from '../stores/session-store';
  import {
    applyListedWindow,
    applyStatusPush,
    createWindowRegistry,
    type WindowEntry,
    type WindowRegistryState,
  } from '../stores/window-registry';
  import { bindPalette } from '../theme/theme-apply';
  import { startDefaultPaletteSync } from '../theme/default-palettes';
  import { isBuiltInSelection, type AppThemeSelection } from '../theme/theme-resolve';
  import { createBrowserThemeDeps, createThemeStore } from '../theme/theme-store';
  import {
    chat,
    dialogs,
    forms,
    errors,
    banners,
    nav,
    themes,
    describeDetectionWarning,
    describeDiscardedKeymapEntries,
    describeEventTime,
    describeOperationFailure,
    describeThemeName,
    foldExternalText,
    plans as plansText,
    settings,
  } from '../ui-messages';
  import { shouldDropRememberedCwd, windowOpenFailureOf } from '../window-open-failure-model';
  import Navigator from './Navigator.svelte';
  import WorkArea from './WorkArea.svelte';

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

  // The mode the picker opened in, or null while it is closed.
  let pickerMode: PickerMode | null = null;
  // From closing the picker on a chosen row until that row starts to run; the
  // permission dialog waits through it so it cannot flash up in between.
  let isPickerRunPending = false;
  // Spelled out: the linter reads a component instance as `any`.
  let commandPicker: { showMode: (mode: PickerMode) => void } | undefined;

  // Restore flow.
  let restoreCandidates: RestoreEntry[] = [];
  let restoreTakenAtEpochS: number | null = null;
  let isRestoreDialogOpen = false;
  let restoreSelection: Set<number> = new Set();
  // Checkboxes the user toggled by hand (position -> checked); never rendered
  // directly, so there is no clearing path -- it lives for the process lifetime.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity -- lookup table read only when the dialog opens, never rendered, so runes-based SvelteMap reactivity does not apply
  const restoreSelectionOverrides = new Map<number, boolean>();
  // What the app says about its own state. Holds what the shell learns itself; what is
  // derived from other state is filled in by `sourceState`.
  let noticeState: NoticeSourceState = {
    daemonSpawnNotice: null,
    detectionWarning: null,
    keymapNotice: null,
    boundaryNotice: null,
    hasThemeLoadFailure: false,
    isThemeLoadFailureDismissed: false,
    themeIssueCount: 0,
    isThemeIssuesDismissed: false,
    approvalExpiredUnseen: null,
    approverRegistrationRefused: false,
    daemonRecovery: 'idle',
    daemonRecoveryRev: 0,
    clientConfigFailure: null,
    isDaemonProtocolStale: false,
    startupEnvFailure: null,
    profilesNeedingNativeProgram: [],
    isProfileUnavailableDismissed: false,
    daemonHealthNotices: [],
    lastSession: null,
    isReopenOfferDismissed: false,
    hasOpenWindows: false,
  };

  // Things that happened, kept until the user dismisses them; `noticeMemory` numbers them.
  let eventLog: EventLog = [];
  let noticeMemory = emptyNoticeMemory();
  // What the lane holds between runs of the feed, handed back as it returned it.
  let laneState = emptyLaneState();
  // The windows calling the user, stepped on each observed arrival and whenever the view or list moves.
  let needsYouToasts: NeedsYouToastState = emptyNeedsYouToastState();
  // The newest notice the user has seen in the Notifications list.
  let seenThrough = -1;
  // Counted where the modal focus port hears a dialog open and close; a reactive value so the
  // pop-ups held behind a modal come up as soon as the last one closes.
  let openModalCount = 0;
  // Pop-up timers stand still while the app is not in front.
  let appFocused = document.hasFocus();
  $: timersRunning = !$appHidden && appFocused;

  let isNoticeCenterOpen = false;
  let noticeCenterAnchor: Rect | null = null;
  // Fixed for one opening, so what came after the last opening stays marked new while it is open.
  let noticeCenterNewerThan = Number.POSITIVE_INFINITY;
  let notificationsButton: HTMLButtonElement | undefined;

  const keyboardPlatform = hostKeyboardPlatform();
  const keymapStore = createKeymapStore({
    platform: keyboardPlatform,
    save: invokeSaveKeymap,
  });
  // How the keymap load ended; the Keyboard tab refuses to edit a file that was not read.
  let keymapLoadStatus: KeymapLoadStatus = { kind: 'not-loaded' };
  // The keymap.toml path this instance reads, `null` until known or when it has none.
  let keymapFilePath: string | null = null;

  // Measured size of the float area; every layout calculation is fed from it
  let area: Size = { width: 0, height: 0 };

  // The work area element tiling measures; `area` tracks the float area inside it.
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
  // `windowId` is the window the operation is about, null for a room or a broadcast.
  let confirmDialog = {
    isOpen: false,
    operation: 'close' as ConfirmOperation,
    roomName: '',
    windowId: null as string | null,
  };
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
  // Each room's generation; a page read under an older one predates a clear.
  let chatRoomGenerations: Map<string, number> = new Map();
  let chatNeedsUser = false;
  let unsubscribeChatChanged: (() => void) | null = null;
  let unsubscribeChatParticipantsChanged: (() => void) | null = null;
  let unsubscribeChatHistoryCleared: (() => void) | null = null;

  // The plan ledger, open independently of the chat panel. Liveness and the
  // owner candidates are derived by the panel from the window registry, so
  // nothing here is read again when only the window list moves.
  let isPlansOpen = false;
  // The tab the plans panel shows: the last one viewed while the app runs, Tickets at first.
  let plansTab: PlansTab = 'tickets';
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
    openTicketNumber = null;
    openTicket = null;
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

  // The Tickets tab: read only, so all it keeps is what was last read for
  // the workspace and milestone chosen, and the ticket whose details are open.
  let ticketWorkspaces: TicketWorkspace[] = [];
  let ticketWorkspaceId: number | null = null;
  let ticketMilestone: string | null = null;
  let ticketOverview: TicketList | null = null;
  let ticketBoard: TicketBoard | null = null;
  let openTicketNumber: number | null = null;
  let openTicket: Ticket | null = null;
  let unsubscribeTicketChanged: (() => void) | null = null;

  // The Settings panel: its own open flag and active tab, plus the state of
  // the one operation it can start (switching a profile on or off).
  let isSettingsOpen = false;
  let settingsTab: SettingsTab = 'profiles';
  const notificationPreferencesStorage = createGuardedLocalStorage(NOTIFICATION_PREFERENCES_KEY);
  let notificationPreferences = readPreferences(notificationPreferencesStorage.read());

  function handleSetNotificationPreference(category: NoticeCategory, enabled: boolean): void {
    notificationPreferences = withPreference(notificationPreferences, category, enabled);
    notificationPreferencesStorage.write(writePreferences(notificationPreferences));
  }
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

  const registry = createWindowRegistry();

  // The work area's state: the current window is derived from it together with
  // the registry, never from the placement alone.
  const shell = writable<ShellState>(
    initialShellState(
      readWorkMode({ getItem: (key: string) => createGuardedLocalStorage(key).read() })
    )
  );

  /**
   * Runs one event through the shell reducer and carries out what it asks for.
   * The keyboard moves only when the reducer says so: the window list changing
   * on its own never moves it. The work mode is saved only when a step changes it.
   */
  function dispatchShell(event: ShellEvent): ShellStep {
    const modeBefore = get(shell).workMode;
    const step = reduceShell(get(shell), event);
    shell.set(step.state);
    if (step.state.workMode !== modeBefore) {
      writeWorkMode(
        { setItem: (key: string, value: string) => createGuardedLocalStorage(key).write(value) },
        step.state.workMode
      );
    }
    for (const effect of step.effects) {
      switch (effect.type) {
        case 'bringToFront': {
          const entry = get(registry).windows.find((e) => e.windowId === effect.windowId);
          if (entry !== undefined) {
            actions.goTo(entry);
          }
          break;
        }
        case 'focusTerminal':
          focusTerminalOf(effect.windowId);
          break;
      }
    }
    return step;
  }

  function selectWindow(windowId: string): void {
    dispatchShell({ type: 'select', windowId });
  }

  /**
   * The view menu's choice; the reducer decides what switching asks for. Choosing is the
   * user's own act, so the keyboard follows to the window the new view shows.
   */
  function chooseWorkMode(mode: WorkMode): void {
    dispatchShell(
      mode === 'focus'
        ? { type: 'enterFocus', canvasCurrent: currentWindowNow() }
        : { type: 'enterCanvas' }
    );
    const shown = currentWindowNow();
    if (shown !== null) {
      focusTerminalOf(shown.windowId);
    }
  }

  function isFocusMode(): boolean {
    return get(shell).workMode === 'focus';
  }

  /** The current window as of now, for handlers that run outside the reactive graph. */
  function currentWindowNow(): WindowEntry | null {
    return currentWindowFor(workViewOf(get(shell)), get(registry).windows);
  }

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
  $: sourceState = {
    ...noticeState,
    hasThemeLoadFailure: themeLoadFailure !== null,
    themeIssueCount,
    profilesNeedingNativeProgram: profilesNeedingNativeProgram(profileOptions),
    lastSession:
      restoreCandidates.length > 0
        ? {
            count: restoreCandidates.length,
            timestamp:
              restoreTakenAtEpochS === null ? null : formatRestoreTimestamp(restoreTakenAtEpochS),
          }
        : null,
    hasOpenWindows: $registry.windows.length > 0,
  } satisfies NoticeSourceState;
  $: noticeDrafts = buildNotices(sourceState);
  // With no window open the work area makes the offer, so the two never make it together.
  $: restoreOffer = reopenOffer(sourceState);
  $: windowNames = new Map($registry.windows.map((entry) => [entry.windowId, entry.displayName]));
  // The lane is an input so a closed or timed-out card runs the feed again; the
  // function writes them back, which a statement reading them could not do itself.
  $: feed = runNoticeFeed(
    noticeDrafts,
    eventLog,
    seenThrough,
    needsYouToasts,
    notificationPreferences,
    openModalCount,
    laneState,
    windowNames,
    isNoticeCenterOpen
  );
  $: toastSecondLines = secondLinesOf(feed, $registry.windows);
  $: closeNoticeCenterBehindModal(openModalCount);
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

  $: currentRoomName = chatRooms.get(currentRoomId)?.displayName ?? currentRoomId;
  $: confirmWording =
    confirmDialog.operation === 'broadcast'
      ? {
          ...confirmCopy(confirmDialog.operation),
          message: chat.broadcastConfirmRoomMessage(currentRoomName),
        }
      : confirmCopy(confirmDialog.operation, confirmDialog.roomName);

  $: approvalRequest = approvalQueue.head(approvals);
  $: isApprovalDialogOpen =
    approvalRequest !== null &&
    isApprovalDialogAllowed({
      isAddDialogOpen,
      isConfirmDialogOpen: confirmDialog.isOpen,
      isRenameDialogOpen: renameDialog.isOpen,
      isRestoreDialogOpen,
      isPickerInUse: pickerMode !== null || isPickerRunPending,
    });
  $: isApprovalRequesterOpen = isRequesterWindowOpen(approvalRequest, $registry.windows);
  // Delegated to a function for the same reason as the folder-drop sync
  // above: the statement would otherwise read what it assigns.
  $: void syncApprovalDisplay(isApprovalDialogOpen, approvalRequest);

  $: workView = workViewOf($shell);
  // The window keyboard operations act on; derived so no second record of it can drift.
  $: current = currentWindowFor(workView, $registry.windows);
  // A call goes once its window is the one being looked at, so moving the view re-steps it.
  $: needsYouToasts = stepCallToasts(needsYouToasts, $registry.windows, current);

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
  const confirmPort: ConfirmPort = (operation, entry) =>
    requestConfirmation(operation, '', entry.windowId);

  function requestConfirmation(
    operation: ConfirmOperation,
    roomName = '',
    windowId: string | null = null
  ): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      // A request can only still be pending if its dialog is the one being
      // replaced here. Deny it instead of leaving its caller waiting forever.
      answerConfirm(false);
      pendingConfirm = resolve;
      confirmDialog = { isOpen: true, operation, roomName, windowId };
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
      pendingFocus.request({ kind: 'panel', panel: 'chat' });
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
    const generation = chatRoomGeneration(chatRoomGenerations, roomId);
    try {
      const page = await invokeReadChat(roomId, afterSeq);
      // Folded into what is held once the page is back, not when it was asked for.
      chatRoomStates = foldChatPage(chatHistory(), roomId, generation, page);
    } catch (error) {
      if (!(await giveWayIfRoomRemoved(roomId, error))) {
        onError(error);
      }
    }
  }

  /**
   * A read can race the deletion of the room it reads. When the refusal was
   * for a room the listing no longer has, the panel moves on as the
   * removal push would, and there is no failure to report.
   */
  async function giveWayIfRoomRemoved(roomId: string, error: unknown): Promise<boolean> {
    await refreshChatRooms();
    if (!readRefusedForRemovedRoom(error, roomId, chatRooms)) {
      return false;
    }
    if (currentRoomId === roomId) {
      selectChatRoom(roomAfterRemoval(currentRoomId, chatRooms));
    }
    return true;
  }

  function chatHistory(): ChatHistory {
    return { states: chatRoomStates, generations: chatRoomGenerations };
  }

  /**
   * One room-list read: a failed one is reported to the console only, the
   * same fail-quiet rule the startup read follows, since there is no
   * obviously-relevant notice for a background list refresh.
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

  /**
   * A room's history was cleared, or the room deleted: what is held for it is
   * dropped and the rooms are listed again even while the panel is closed,
   * since a clear also lifts the room's attention badge. A shown room that
   * is gone gives way to `main`.
   */
  async function applyChatHistoryClearedPush(payload: ChatHistoryCleared): Promise<void> {
    const cleared = applyChatHistoryCleared(chatHistory(), payload.room_id);
    chatRoomStates = cleared.states;
    chatRoomGenerations = cleared.generations;
    await refreshChatRooms();
    const nextRoomId = roomAfterRemoval(currentRoomId, chatRooms);
    if (!isChatOpen) {
      currentRoomId = nextRoomId;
      return;
    }
    if (nextRoomId !== currentRoomId) {
      selectChatRoom(nextRoomId);
    } else if (currentRoomId === payload.room_id) {
      void refreshChat();
    }
  }

  /**
   * Clears or deletes a room once the user confirms. What the panel holds
   * changes on the `chat_history_cleared` push that follows, the same way
   * for a removal made here or by another approver.
   */
  async function removeChatRoom(
    operation: 'clear_room' | 'delete_room',
    room: ChatRoomInfo
  ): Promise<void> {
    if (!(await requestConfirmation(operation, room.displayName))) {
      return;
    }
    try {
      await (operation === 'clear_room'
        ? invokeClearChatRoom(room.room_id)
        : invokeDeleteChatRoom(room.room_id));
    } catch (error) {
      console.error(chat.panelTitle, error);
      recordFailure('operation-failure', describeRoomRemovalFailure(error));
      void refreshChatRooms();
    }
  }

  /** Posts one message; `null` leaves the composer holding what was typed. */
  async function postChat(body: string, broadcast: boolean): Promise<ChatPosted | null> {
    const roomId = currentRoomId === '' ? undefined : currentRoomId;
    let posted: ChatPosted;
    try {
      posted = broadcast
        ? await invokeBroadcastChat(body, roomId)
        : await invokeSendChat(body, roomId);
    } catch (error) {
      reportOperationFailure(chat.panelTitle, error);
      return null;
    }
    await refreshChat();
    return posted;
  }

  async function broadcastChat(body: string): Promise<ChatPosted | null> {
    if (!(await requestConfirmation('broadcast'))) {
      return null;
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
      pendingFocus.request({ kind: 'panel', panel: 'plans' });
      void readTicketWorkspaces();
      await applyPlanReload({ list: true, roles: true, planId: selectedPlanId });
      if (planBoardBecameVisible({ open: false, tab: plansTab }, { open: true, tab: plansTab })) {
        notePlanViewed();
      }
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
    pendingFocus.request({ kind: 'panel', panel: 'settings' });
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
   * a push re-reading the plan is not a look, and neither is a plan read
   * while another tab of the panel is shown.
   */
  function notePlanViewed(): void {
    if (!isPlansOpen || plansTab !== 'board') {
      return;
    }
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
    selectPlansTab('models');
    requestedModelProfile = { id, seq: (requestedModelProfile?.seq ?? 0) + 1 };
  }

  /**
   * Shows one tab of the plans panel. Pressing the Board tab counts as
   * looking at the plan, even when it was already shown.
   */
  function selectPlansTab(tab: PlansTab): void {
    const previous = { open: isPlansOpen, tab: plansTab };
    plansTab = tab;
    closeTicket();
    if (tab === 'tickets') {
      // Reads the board too, so a workspace that appeared meanwhile is listed.
      void readTicketWorkspaces();
    }
    if (
      planBoardBecameVisible(previous, { open: isPlansOpen, tab }) ||
      (previous.tab === 'board' && tab === 'board')
    ) {
      notePlanViewed();
    }
  }

  /** Reads the workspaces that have tickets, and keeps or picks the one shown. */
  async function readTicketWorkspaces(): Promise<void> {
    let workspaces: TicketWorkspace[];
    try {
      workspaces = await invokeListTicketWorkspaces();
    } catch (error) {
      reportOperationFailure(plansText.ticketsTab, error);
      return;
    }
    ticketWorkspaces = workspaces;
    const shown = ticketWorkspaceId;
    if (shown === null || !workspaces.some((entry) => entry.workspace_id === shown)) {
      selectTicketWorkspace(workspaces[0]?.workspace_id ?? null);
    } else {
      void refreshTicketBoard();
    }
  }

  function selectTicketWorkspace(workspaceId: number | null): void {
    ticketWorkspaceId = workspaceId;
    ticketMilestone = null;
    ticketOverview = null;
    ticketBoard = null;
    closeTicket();
    void refreshTicketBoard();
  }

  function selectTicketMilestone(milestone: string | null): void {
    ticketMilestone = milestone;
    void refreshTicketBoard();
  }

  /**
   * Reads the board of the workspace shown: one read for the settings,
   * milestones and tickets in states the settings no longer name, then one
   * read per state. The answer is dropped if the choice changed meanwhile.
   */
  async function readTicketBoardOnce(): Promise<void> {
    const workspaceId = ticketWorkspaceId;
    const milestone = ticketMilestone;
    if (workspaceId === null) {
      return;
    }
    try {
      const overview = await invokeListTickets({
        workspaceId,
        ...(milestone === null ? {} : { milestone }),
      });
      const config = overview.config;
      const columns =
        config === null
          ? []
          : await Promise.all(
              ticketColumnQueries(workspaceId, config, milestone).map(
                async (query) => [query.state ?? '', await invokeListTickets(query)] as const
              )
            );
      if (ticketWorkspaceId !== workspaceId || ticketMilestone !== milestone) {
        return;
      }
      ticketOverview = overview;
      ticketBoard = buildTicketBoard(overview, new Map(columns));
    } catch (error) {
      if (ticketWorkspaceId === workspaceId) {
        reportOperationFailure(plansText.ticketsTab, error);
      }
      return;
    }
    if (openTicketNumber !== null) {
      await readOpenTicket();
    }
  }

  /** Serialized so concurrent pushes cannot pile up board reads. */
  const refreshTicketBoard = serializeRefresh(readTicketBoardOnce);

  function openTicketCard(ticket: number): void {
    openTicketNumber = ticket;
    openTicket = null;
    void readOpenTicket();
  }

  function closeTicket(): void {
    openTicketNumber = null;
    openTicket = null;
  }

  /** Reads the open ticket in full; a failed read goes back to the board. */
  async function readOpenTicket(): Promise<void> {
    const workspaceId = ticketWorkspaceId;
    const ticket = openTicketNumber;
    if (workspaceId === null || ticket === null) {
      return;
    }
    const isStillOpen = (): boolean =>
      ticketWorkspaceId === workspaceId && openTicketNumber === ticket;
    try {
      const next = await invokeGetTicket(workspaceId, ticket);
      if (isStillOpen()) {
        openTicket = next;
      }
    } catch (error) {
      if (isStillOpen()) {
        reportOperationFailure(plansText.ticketsTab, error);
        closeTicket();
      }
    }
  }

  /** Takes the user from a ticket's details to one of its plans on the Board tab. */
  function openTicketPlan(planId: string): void {
    // Selected first, so the Board tab coming into view is never counted as
    // a look at the plan shown before.
    selectPlan(planId);
    selectPlansTab('board');
  }

  /**
   * A `ticket_changed` push is read only while the Tickets tab is shown:
   * there is no badge to keep current, and showing the tab reads it again.
   */
  function applyTicketChanged(payload: TicketChanged): void {
    if (!isPlansOpen || plansTab !== 'tickets') {
      return;
    }
    const reload = ticketReloadFor(
      payload,
      ticketWorkspaceId,
      ticketWorkspaces.map((entry) => entry.workspace_id)
    );
    if (reload.workspaces) {
      void readTicketWorkspaces();
    }
    if (reload.board) {
      void refreshTicketBoard();
    }
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
   * displayed is counted for its notice instead of disappearing without the
   * user ever learning it was asked -- but only when it actually timed out:
   * a decision made elsewhere, or the requester
   * disconnecting, is not an unnoticed expiry.
   */
  function withdrawApproval(requestId: string, reason: WithdrawReason): void {
    const request = approvals.items.find((entry) => entry.request_id === requestId);
    if (request !== undefined) {
      noticeState = recordApprovalExpiredUnseen(
        noticeState,
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
      );
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
        approvals = approvalQueue.failDecision(approvals, requestId);
        recordFailure('operation-failure', banners.approvalDecisionFailed);
      });
  }

  const contractedActions = createWindowActions({
    registry,
    ipc: { invokeCloseWindow, invokeInterruptWindow, invokeRenameWindow },
    confirmPort,
    removeWindow: removeClosedWindow,
    closeRequested: (windowId) => {
      dispatchShell({ type: 'closeRequested', windowId });
    },
    closeFailed: (windowId) => {
      dispatchShell({ type: 'closeFailed', windowId });
    },
    selectWindow,
    workMode: () => get(shell).workMode,
  });

  // The view starts these operations without awaiting them, so a refusal has
  // to become a notice here, where the notices are owned. Rename is awaited by
  // its dialog, which shows the refusal itself.
  // Maximizing and minimizing are the user's own choice of front window, so the
  // keyboard follows (activateWindow).
  // In Focus, going to a window only chooses the one Focus shows, and minimize
  // and maximize do nothing: Focus never writes the canvas placement.
  const actions: WindowActions = {
    ...contractedActions,
    close: (entry) => reportIfFailed(errors.closeWindowFailed, contractedActions.close(entry)),
    interrupt: (entry) =>
      reportIfFailed(errors.interruptWindowFailed, contractedActions.interrupt(entry)),
    minimize: (entry) => {
      if (isFocusMode()) {
        return;
      }
      contractedActions.minimize(entry);
      returnFocusIfLost();
    },
    maximize: (entry) => {
      if (isFocusMode()) {
        return;
      }
      contractedActions.maximize(entry);
      focusTerminalOf(entry.windowId);
    },
  };

  // Every jump to a window reads the rooms as the sidebar renders them, and
  // expands through the store's idempotent `expand` so a room already open is
  // never closed.
  const revealDeps: RevealDeps<WindowEntry> = {
    groups: sidebarGroups,
    expandRoom: collapsedRooms.expand,
    goTo: actions.goTo,
    focusTerminal: focusTerminalOf,
  };
  // What the Needs you filter lists, `null` while it is off. Only the user's
  // own acts rebuild it, so its rows never move on their own.
  let needsYouView: NeedsYouView | null = null;

  const attentionNavigation = createAttentionNavigation({
    ...revealDeps,
    current: currentWindowNow,
    view: () => needsYouView,
  });

  function toggleNeedsYouFilter(): void {
    needsYouView = needsYouView === null ? nextNeedsYouView(null, sidebarGroups(), true) : null;
  }

  function advanceNeedsYouView(
    userOperated: boolean,
    groups: readonly RoomGroup<WindowEntry>[] = sidebarGroups()
  ): void {
    if (needsYouView !== null) {
      needsYouView = nextNeedsYouView(needsYouView, groups, userOperated);
    }
  }

  function goToNextWindowNeedingYou(): WindowEntry | null {
    const target = attentionNavigation.goToNextWindowNeedingYou();
    advanceNeedsYouView(true);
    return target;
  }

  /** A window the user chose, in the sidebar or the picker: it comes on screen and takes the keyboard. */
  function goToWindowByUser(entry: WindowEntry): void {
    revealWindow(revealDeps, entry);
    advanceNeedsYouView(true);
  }

  function openChatForRoom(roomId: string): void {
    if (isChatOpen) {
      selectChatRoom(roomId);
      return;
    }
    currentRoomId = roomId;
    void toggleChat();
  }

  const FOCUSABLE =
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  let sidebarElement: HTMLElement | null = null;

  $: sidebarGroupsNow = groupByRoom($registry.windows, $collapsedRooms, $registry.tabOrder);
  $: advanceNeedsYouView(false, sidebarGroupsNow);
  // A request names the window that asked; that window's row wears the mark.
  $: permissionWaiters = approvalQueue.windowsAwaitingPermission(approvals);

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
      case 'sidebar-row': {
        const row = sidebarRow(move.windowId);
        if (row !== null) {
          focusAsLastInput(row);
        }
        return;
      }
      case 'sidebar-start': {
        const first = firstFocusable(sidebarElement);
        if (first !== null) {
          focusAsLastInput(first);
        }
        return;
      }
      case 'current-if-lost':
        runFocusMove(lostFocusMove(focusFacts(document.activeElement), currentWindowNow()));
        return;
      case 'none':
        return;
    }
  }

  /** The one place the DOM is read for `isInputTarget` and `lostFocusMove`. */
  function focusFacts(element: Element | null): FocusFacts {
    if (element === null || element === document.body) {
      return {
        onBody: true,
        connected: false,
        rendered: false,
        disabled: false,
        onWindowControl: false,
      };
    }
    return {
      onBody: false,
      connected: element.isConnected,
      rendered: element.getClientRects().length > 0,
      disabled: element.matches(':disabled'),
      onWindowControl: element.closest('[data-window-control]') !== null,
    };
  }

  type FocusReturn = PanelReturn<PanelId, HTMLElement>;

  /** Performs one return: an element, the start of a panel, or a focus move. */
  function runFocusReturn(target: FocusReturn): void | Promise<void> {
    switch (target.kind) {
      case 'element':
        focusAsLastInput(target.element);
        return;
      case 'panel':
        return focusPanelStart(
          PANEL_SELECTORS[target.panel],
          target.panel === 'chat' ? '.chat-input' : FOCUSABLE
        );
      case 'move':
        runFocusMove(target.move);
        return;
    }
  }

  // Every focus move goes through one held request that runs once no modal
  // dialog is open, after the view has rendered (a hidden window cannot take focus).
  const pendingFocus = createPendingFocus<FocusReturn>({
    isModalOpen: hasOpenModal,
    settle: tick,
    run: runFocusReturn,
  });

  function moveFocus(move: FocusMove): void {
    pendingFocus.request({ kind: 'move', move });
  }

  /** Points the keyboard at a window brought to the front; see `windowActivationMove`. */
  function activateWindow(input: Parameters<typeof windowActivationMove>[0]): void {
    const move = windowActivationMove(input);
    if (move.kind !== 'none') {
      moveFocus(move);
    }
  }

  /** Asks for keyboard focus to move into the terminal of a window the user chose. */
  function focusTerminalOf(windowId: string): void {
    activateWindow({ byUser: true, target: windowId, pressOnControl: false });
  }

  /** The rail's Windows item: the keyboard goes back to the current window, when there is one. */
  function focusCurrentTerminal(): void {
    const now = currentWindowNow();
    if (now !== null) {
      focusTerminalOf(now.windowId);
    }
  }

  /** After the user changed the front window without one target: back only if focus was lost. */
  function returnFocusIfLost(): void {
    activateWindow({ byUser: true, target: null, pressOnControl: false });
  }

  /** Drops a window whose close the user confirmed, then returns focus if it went with it. */
  function removeClosedWindow(windowId: string): void {
    closePane(windowId);
    returnFocusIfLost();
  }

  /** Where focus is now, remembered by a modal dialog that is opening. */
  function modalOrigin(): ModalOrigin<PanelId, HTMLElement> {
    const element = document.activeElement;
    if (!(element instanceof HTMLElement) || element === document.body) {
      return { element: null, panel: lastFocusedPanel };
    }
    return { element, panel: panelContaining(element) };
  }

  // The dialogs that have opened and not yet reported closing, by token.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity -- read only when a dialog closes, never rendered, so runes-based SvelteMap reactivity does not apply
  const openModals = new Map<
    ModalFocusToken,
    { subject: string | null; origin: ModalOrigin<PanelId, HTMLElement> }
  >();
  let nextModalToken: ModalFocusToken = 0;

  provideModalFocusPort({
    opened: (subject) => {
      const token = nextModalToken;
      nextModalToken += 1;
      openModals.set(token, { subject, origin: modalOrigin() });
      openModalCount += 1;
      return token;
    },
    closed: (token) => {
      const opened = openModals.get(token);
      if (opened === undefined) {
        return;
      }
      openModals.delete(token);
      openModalCount -= 1;
      // An offer: a move asked for while the dialog was open, or the return of
      // a dialog that closed before it, wins.
      pendingFocus.offer(
        modalReturnTarget<PanelId, HTMLElement>({
          subject: opened.subject,
          origin: opened.origin,
          current: currentWindowNow(),
          isUsable: (element: HTMLElement) => isInputTarget(focusFacts(element)),
          isOpen: isPanelOpen,
        })
      );
    },
  });

  /** Moves focus between the sidebar and the current window; it depends only on where focus is now. */
  function toggleSidebarFocus(): void {
    const focusInSidebar = sidebarElement?.contains(document.activeElement) ?? false;
    moveFocus(
      sidebarToggleMove(
        focusInSidebar,
        currentWindowNow(),
        (windowId: string) => sidebarRow(windowId) !== null
      )
    );
  }

  /** The sidebar's rooms as it renders them, top to bottom. */
  function sidebarGroups(): RoomGroup<WindowEntry>[] {
    return groupByRoom(get(registry).windows, get(collapsedRooms), get(registry).tabOrder);
  }

  // The sidebar order as of the last registry change: the order a vanished
  // window's next window is read from. Which windows left or joined is the
  // shell-state diff's call, not this view's.
  let listedBefore: OrderedWindow[] = [];

  function observeWindowList(state: WindowRegistryState): void {
    const listed: OrderedWindow[] = windowsInDisplayOrder(sidebarGroups()).map((entry) => ({
      windowId: entry.windowId,
      name: entry.displayName,
      exitCode: null,
    }));
    // A window's code is the one the update removing it retired it with; a
    // window closed any other way has none.
    const retiredCodes = new Map(state.retired.map((gone) => [gone.windowId, gone.exitCode]));
    const events = [
      ...diffGoneWindows(listedBefore, listed).map((event) => ({
        ...event,
        exitCode: retiredCodes.get(event.windowId) ?? null,
      })),
      ...diffAppearedWindows(listedBefore, listed),
    ];
    listedBefore = listed;
    for (const event of events) {
      const step = dispatchShell(event);
      if (event.type === 'windowGone') {
        const exit = windowExitEvent({
          ...event,
          goneByUser: step.goneByUser === true,
          carded: step.state.exitedCard?.windowId === event.windowId,
        });
        if (exit !== null) {
          recordEvent(exit);
        }
      }
    }
  }

  const unsubscribeListObservation = registry.subscribe(observeWindowList);

  // What the core is told about the things waiting for the user. Arrivals are
  // gathered into one mark that the next send carries and clears.
  let arrivalMemory = emptyArrivalMemory();
  let attentionSend = emptyAttentionSendState();

  // Every registry change is observed, not only the last one of a tick: a
  // `running` applied and replaced within one tick still opens a turn.
  function observeArrivals(state: WindowRegistryState): void {
    const observed = observeNeedsYouArrivals(arrivalMemory, state.windows);
    arrivalMemory = observed.memory;
    if (observed.arrived.length > 0) {
      attentionSend = noteAttentionArrival(attentionSend);
    }
    needsYouToasts = stepCallToasts(
      needsYouToasts,
      state.windows,
      currentWindowFor(workViewOf(get(shell)), state.windows),
      observed.arrived
    );
    sendAttentionSummary(state, approvals, false);
  }

  /** One step of the window calls over `windows`, read through the same `needsYou` as every list. */
  function stepCallToasts(
    state: NeedsYouToastState,
    windows: readonly WindowEntry[],
    seen: WindowEntry | null,
    arrived: readonly string[] = [],
    dismissed: string | null = null
  ): NeedsYouToastState {
    return stepNeedsYouToasts(state, {
      arrived,
      windows: windows.map((entry) => ({
        windowId: entry.windowId,
        status: entry.detected.status,
        needsYou: needsYou(entry),
      })),
      seenWindowId: seen?.windowId ?? null,
      ...(dismissed === null ? {} : { dismissed }),
    });
  }

  /** Sends when the totals moved or an arrival is pending, or always when `always`. */
  function sendAttentionSummary(
    registryState: WindowRegistryState,
    queue: approvalQueue.ApprovalQueueState,
    always: boolean
  ): void {
    const began = beginAttentionSend(
      attentionSend,
      {
        needsYou: attentionCount(registryState.windows),
        pendingPermissions: approvalQueue.size(queue),
      },
      always
    );
    attentionSend = began.state;
    const { send } = began;
    if (send === null) {
      return;
    }
    invokeSetAttentionSummary(send.needsYou, send.pendingPermissions, send.arrived).then(
      () => {
        attentionSend = settleAttentionSend(attentionSend, send, true);
      },
      (error: unknown) => {
        attentionSend = settleAttentionSend(attentionSend, send, false);
        console.error('failed to set the attention summary:', error);
      }
    );
  }

  const unsubscribeArrivals = registry.subscribe(observeArrivals);
  $: sendAttentionSummary(get(registry), approvals, false);

  const runnerDeps: ShortcutRunnerDeps<WindowEntry> = {
    current: currentWindowNow,
    workMode: () => get(shell).workMode,
    exitedCard: () => get(shell).exitedCard,
    displayOrder: () => visibleWindowOrder(sidebarGroups(), needsYouView),
    groups: sidebarGroups,
    expandRoom: collapsedRooms.expand,
    actions,
    goToNextWindowNeedingYou,
    nextAttention: attentionNavigation.peekNextWindowNeedingYou,
    statusOf: (entry) => entry.detected.status,
    focusTerminal: focusTerminalOf,
    openWindowForm: openAddDialog,
    renameWindow: handleRenameWindow,
    openPicker: (mode) => {
      pickerMode = mode;
    },
    toggleWorkMode: () => {
      chooseWorkMode(isFocusMode() ? 'canvas' : 'focus');
    },
    tileWindows: handleArrangeWindows,
    toggleNeedsYouFilter,
    toggleChat,
    togglePlans,
    toggleSettings,
    toggleNotifications: toggleNoticeCenter,
    toggleSidebarFocus,
  };
  const runShortcut = createShortcutRunner(runnerDeps);

  /** The one way an action runs, from a key or from the picker; a failure shows as a notice. */
  function runAction(action: ActionId): Promise<void> {
    return reportIfFailed(errors.shortcutFailed, runShortcut(action));
  }

  /** Whether `action` would do anything now, judged from what the runner itself reads. */
  function availabilityOf(action: ActionId): ActionVerdict {
    return actionAvailability(action, availabilityContext(runnerDeps));
  }

  // `$:` tracks only names written in the statement, and the runner's deps read through get(),
  // so every source those deps read is listed here.
  let tileAvailability: ActionVerdict;
  $: {
    void [$shell, $registry, $collapsedRooms, needsYouView];
    tileAvailability = availabilityOf('tile-windows');
  }

  // The window-level capture listener below is the only place keys are judged as
  // shortcuts, so it sees every key before the terminal and the inputs do.
  const shortcutDispatcher = createShortcutDispatcher({
    bindings: () => get(keymapStore).bindings,
    isModalOpen: hasOpenModal,
    run: (action) => {
      void runAction(action);
    },
    platform: keyboardPlatform,
  });

  $: pickerWindows = windowsInDisplayOrder(sidebarGroupsNow);

  type PickerChoice = { kind: 'window'; windowId: string } | { kind: 'action'; action: ActionId };

  /**
   * Closes the picker, then starts the chosen row once the picker's focus return
   * has run, so the row acts from where focus was before the picker opened.
   */
  async function runFromPicker(choice: PickerChoice): Promise<void> {
    pickerMode = null;
    isPickerRunPending = true;
    try {
      await tick();
      await pendingFocus.settled();
      if (choice.kind === 'window') {
        const entry = get(registry).windows.find((w) => w.windowId === choice.windowId);
        if (entry !== undefined) {
          goToWindowByUser(entry);
        }
      } else {
        // Not awaited: a dialog the action opens is marked before this returns,
        // and the permission dialog must not wait for the daemon's reply.
        void runAction(choice.action);
      }
    } finally {
      isPickerRunPending = false;
    }
  }

  function chooseWindowInPicker(entry: WindowEntry): void {
    void runFromPicker({ kind: 'window', windowId: entry.windowId });
  }

  function chooseActionInPicker(action: ActionId): void {
    // Going to a window from the commands keeps the picker open in its windows mode.
    if (action === 'open-window-picker') {
      commandPicker?.showMode('windows');
      return;
    }
    void runFromPicker({ kind: 'action', action });
  }

  function cancelPicker(): void {
    pickerMode = null;
  }

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
      current: currentWindowNow(),
    });
    panelOrigins.delete(panel);
    recentPanels = recentPanels.filter((open) => open !== panel);
    if (lastFocusedPanel === panel) {
      lastFocusedPanel = null;
    }
    if (shouldReturnFocus) {
      pendingFocus.request(target);
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
    const target = firstFocusable(document.querySelector(panelSelector), targetSelector);
    if (target !== null) {
      focusAsLastInput(target);
    }
  }

  // Says nothing when the operation succeeds.
  async function reportIfFailed(summary: string, operation: Promise<void>): Promise<void> {
    try {
      await operation;
    } catch (error) {
      reportOperationFailure(summary, error);
    }
  }

  onDestroy(() => {
    unsubscribeListObservation();
    unsubscribeArrivals();
    unbindAppPalette();
    stopDefaultPaletteSync?.();
    stopLayoutSync();
    unsubscribeSession?.();
    void framePull?.stop();
    unsubscribeWindowList?.();
    unsubscribeChatChanged?.();
    unsubscribeChatParticipantsChanged?.();
    unsubscribeChatHistoryCleared?.();
    unsubscribePlanChanged?.();
    unsubscribeTicketChanged?.();
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
    // Set by the first announcement heard: from then on the events carry every
    // later start, so the start-up read of the last one must not overwrite them.
    let isDaemonConnectionHeard = false;
    try {
      unsubscribeSession = await subscribeSession(registry.resolveWindowHandlers, {
        onStatus: (payload) => {
          applyStatusPush(registry, protocol_revision, payload);
        },
        onDaemonConnected: (payload) => {
          isDaemonConnectionHeard = true;
          void rereadDaemonFacts(payload);
        },
        onBoundaryError: (error) => {
          reportBoundaryNotice('sessionEvent', error);
        },
        onApprovalRequested: (payload) => {
          if (!approvalQueue.holds(approvals, payload.request.request_id)) {
            attentionSend = noteAttentionArrival(attentionSend);
          }
          approvals = approvalQueue.enqueue(approvals, payload.request);
        },
        onApprovalWithdrawn: (payload) => {
          withdrawApproval(payload.request_id, payload.reason);
        },
        onApprovalReset: (payload) => {
          approvals = approvalQueue.reset(approvals, payload.requests);
          noticeState = { ...noticeState, approverRegistrationRefused: false };
        },
        onApprovalRegistrationRefused: () => {
          noticeState = { ...noticeState, approverRegistrationRefused: true };
        },
        onDaemonRecovery: (payload) => {
          applyRecovery(payload);
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
      applyRecovery({ status: report.status, rev: report.rev });
      noticeState = {
        ...applyClientConfigFailure(noticeState, report.config_failure),
        startupEnvFailure: report.startup_env_failure,
      };
    } catch (error) {
      console.error('failed to pull the daemon recovery status:', error);
    }

    await refreshWindowList();
    if (isFocusMode()) {
      // Not the user's choice, so the keyboard stays where it is.
      const first = firstWindowAtStartup(
        windowsInDisplayOrder(sidebarGroups()),
        currentWindowFor({ mode: 'canvas' }, get(registry).windows)
      );
      if (first !== null) {
        selectWindow(first.windowId);
      }
    }

    unsubscribeWindowList = await subscribeWindowList(
      (payload) => {
        protocol_revision = payload.protocol_revision;
        noticeState = {
          ...noticeState,
          isDaemonProtocolStale:
            protocol_revision !== null && protocol_revision < ATTACH_VIEW_MIN_REVISION,
        };
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

    unsubscribeChatHistoryCleared = await subscribeChatHistoryCleared(
      (payload) => void applyChatHistoryClearedPush(payload),
      (error) => {
        reportBoundaryNotice('chatChanged', error);
      }
    );

    // A dropped plan push leaves the board at most one push behind, and the
    // next push or reopening the panel reads it again.
    unsubscribePlanChanged = await subscribePlanChanged(applyPlanChanged, (error) => {
      console.error('planChanged boundary error:', error);
    });

    // A dropped ticket push leaves the board at most one push behind, and the
    // next push or showing the Tickets tab again reads it.
    unsubscribeTicketChanged = await subscribeTicketChanged(applyTicketChanged, (error) => {
      console.error('ticketChanged boundary error:', error);
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
        noticeState = { ...noticeState, approverRegistrationRefused: false };
      } else {
        noticeState = { ...noticeState, approverRegistrationRefused: true };
      }
    } catch (error) {
      // Keeping whatever the events already delivered: with no daemon to pull
      // from, the poll's own registration emits the list once one is running.
      console.error('failed to pull the pending approvals:', error);
    }

    // After every subscription: a push only ever carries a later state than
    // the read, and the start's warning may have been announced before the
    // session subscription opened.
    let connection: DaemonConnectedPayload | null = null;
    try {
      const announced = await invokeGetDaemonConnection();
      connection = isDaemonConnectionHeard ? null : announced;
    } catch (error) {
      console.error('failed to pull the daemon connection:', error);
    }
    await rereadDaemonFacts(connection);

    // Unconditional: the core learns this shell's totals even when nothing
    // moved while starting.
    sendAttentionSummary(get(registry), approvals, true);
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
        noticeState = { ...noticeState, keymapNotice: banners.keymapLoadFailed };
        return;
      }
      const { discarded } = keymapStore.load(loaded);
      if (discarded > 0) {
        noticeState = { ...noticeState, keymapNotice: describeDiscardedKeymapEntries(discarded) };
      }
    } catch (error) {
      console.error('failed to load keymap:', error);
      keymapLoadStatus = { kind: 'rejected' };
      noticeState = { ...noticeState, keymapNotice: banners.keymapLoadFailed };
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
    noticeState = { ...noticeState, boundaryNotice: describeBoundaryNotice({ operation, detail }) };
  }

  /**
   * The one place that logs a boundary failure to the console.
   */
  function logBoundaryFailure(operation: BoundaryOperation, detail: unknown): void {
    console.error(`${operation} boundary error:`, detail);
  }

  /** Records one thing that happened, numbered from the counter the feed numbers from. */
  function recordEvent(event: NewEvent): void {
    const next = appendEvent(eventLog, noticeMemory, event, describeEventTime(new Date()));
    eventLog = next.log;
    noticeMemory = next.memory;
  }

  /** Records an event that reports an action that did not go through. */
  function recordFailure(key: FailureEventKey, text: string): void {
    recordEvent(failureEvent(key, text));
  }

  /** Folds a recovery status change in; recovery reaching the daemon again is an event. */
  function applyRecovery(update: DaemonRecoveryUpdate): void {
    const before = noticeState.daemonRecovery;
    noticeState = applyRecoveryUpdate(noticeState, update);
    const reconnected = daemonReconnectedEvent(before, noticeState.daemonRecovery);
    if (reconnected !== null) {
      recordEvent(reconnected);
    }
  }

  /**
   * Records a refused window operation. Operations started here are not put to
   * a human approver -- this GUI is the approver -- so a refusal is a real
   * failure, including the one the core reports on its own when approver
   * registration has not succeeded yet (`ApproverNotRegistered`).
   */
  function reportOperationFailure(summary: string, error: unknown): void {
    console.error(summary, error);
    recordFailure('operation-failure', describeOperationFailure(summary, error));
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
   * Fetches the restore candidates, which a daemon start decides. A failure
   * keeps what was read last and records `banners.restoreLoadFailed`.
   */
  async function loadRestoreCandidates(): Promise<void> {
    try {
      const result = await invokeListRestoreCandidates();
      restoreCandidates = result.candidates;
      restoreTakenAtEpochS = result.takenAtEpochS;
    } catch (error) {
      console.error('failed to list restore candidates:', error);
      recordFailure('restore-load-failed', banners.restoreLoadFailed);
      return;
    }
    // A position counts among the rows of this read, so a choice made against
    // an earlier read would land on another candidate.
    restoreSelectionOverrides.clear();
    if (isRestoreDialogOpen) {
      restoreSelection = defaultSelection(restoreCandidates, restoreOpenWindows);
    }
  }

  /**
   * Reads what belongs to one daemon start and is pushed neither again to a
   * new connection nor across a restart: the health, the profiles' usability,
   * the rooms and plans with their badges, and the restore candidates.
   * Sequential, so the reads do not compete for the one control connection.
   */
  async function rereadDaemonFactsOnce(): Promise<void> {
    try {
      const problems = await invokeGetDaemonHealth();
      noticeState = { ...noticeState, daemonHealthNotices: daemonProblemNotices(problems) };
    } catch (error) {
      console.error('failed to pull the daemon health:', error);
    }
    await loadProfiles();
    await refreshChatRooms();
    await refreshPlanList();
    await loadRestoreCandidates();
  }

  const rereadDaemonStart = serializeRefresh(rereadDaemonFactsOnce);

  /**
   * Takes in one daemon start: its spawn warning replaces the last start's,
   * and its facts are read again. `null` names no start, so the warning is
   * left as it is.
   */
  function rereadDaemonFacts(connection: DaemonConnectedPayload | null): Promise<void> {
    if (connection !== null) {
      const warning = connection.spawn_warning;
      noticeState = {
        ...noticeState,
        daemonSpawnNotice: warning === null ? null : foldExternalText(warning),
      };
    }
    return rereadDaemonStart();
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

  function dismissReopenOffer(): void {
    noticeState = dismissNotice(noticeState, 'reopen-offer');
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
        onNameTaken: launch.onNameTaken,
      });
    }
    return invokeOpenWindow({
      command: launch.command,
      cwd: launch.cwd,
      name: launch.name,
      detection: launch.detection ?? undefined,
      vendor: launch.vendor ?? undefined,
      onNameTaken: launch.onNameTaken,
    });
  }

  async function handleRestoreSelected(): Promise<void> {
    const selected = restoreCandidates
      .filter((candidate) => restoreSelection.has(candidate.position))
      .sort((a, b) => a.position - b.position);
    isRestoreDialogOpen = false;
    dismissReopenOffer();

    const { opened, failed } = await restoreSelected(selected, invokeRestoreLaunch);
    for (const { name, failure } of failed) {
      console.error('failed to restore window:', name, failure);
    }
    const targets: LayoutApplicationTarget[] = opened.map(({ candidate, windowId }) => ({
      windowId,
      position: candidate.position,
      layout: candidate.layout,
    }));
    await refreshWindowList();
    applyLayoutSteps(buildLayoutTarget(), planLayoutApplication(targets, get(registry).tabOrder));
    returnFocusIfLost();
    const notice = restoreNoticeForFailures(failed);
    if (notice !== null) {
      recordFailure('restore-not-restored', notice);
    }
  }

  /** Brings the panes in line with one window-list snapshot; exited windows are retired, never opened. */
  async function applyWindowList(windows: WindowInfo[]): Promise<void> {
    const liveWindows: WindowInfo[] = [];
    for (const info of windows) {
      if (applyListedWindow(registry, protocol_revision, info)) {
        liveWindows.push(info);
      }
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
   * Measures the work area for tiling. Shared by `handleArrangeWindows` and the per-room tile
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
  }

  /**
   * Opens the window the dialog submitted. A refusal is left to propagate:
   * the dialog is where the rejected input still is, so that is where it is
   * shown, and the dialog stays open on it.
   */
  async function handleOpenWindow(input: OpenWindowSubmission): Promise<void> {
    noticeState = { ...noticeState, detectionWarning: null };
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
    const { windowId, detectionWarning: warning } = opened;
    // The window is open either way; a warning only says its status will
    // stay `Unknown` because the definition could not be loaded.
    if (warning !== null) {
      noticeState = { ...noticeState, detectionWarning: describeDetectionWarning(warning) };
    }
    // The pane is created from the window list, the same path the core's
    // poll takes, so opening twice cannot produce two panes for one window.
    await refreshWindowList();
    // The user opened it, so go to it and let the keyboard follow.
    if (get(registry).windows.some((entry) => entry.windowId === windowId)) {
      dispatchShell({ type: 'openedByUser', windowId });
    }
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

  function runNoticeFeed(
    drafts: NoticeFeedInput['drafts'],
    log: EventLog,
    seen: number,
    calls: NeedsYouToastState,
    prefs: NoticeFeedInput['prefs'],
    modals: number,
    lane: NoticeFeedInput['lane'],
    names: NoticeFeedInput['windowNames'],
    listOpen: boolean
  ): NoticeFeedOutput {
    const input: NoticeFeedInput = {
      drafts,
      log,
      memory: noticeMemory,
      seenThrough: seen,
      needsYou: calls,
      prefs,
      openModalCount: modals,
      lane,
      // The lane is not measured yet, so no card shows; arrivals stay on the lane and are read.
      capacity: null,
      windowNames: names,
    };
    let output = noticeFeed(input);
    // What arrives while the list is open is in front of the user, so it is seen. The feed is
    // pure, so running it again on the same input with the new mark only moves what the mark decides.
    const listed = Math.max(seen, ...output.center.map((notice) => notice.seq));
    if (listOpen && listed > seen) {
      seenThrough = listed;
      output = noticeFeed({ ...input, seenThrough: listed });
    }
    noticeMemory = output.memory;
    laneState = output.lane.state;
    return output;
  }

  /**
   * The line under each pop-up's title: for a notice, the detail of the latest notice it holds;
   * for a window call, the place the sidebar lists the window under.
   */
  function secondLinesOf(
    output: NoticeFeedOutput,
    windows: readonly WindowEntry[]
  ): ReadonlyMap<string, string> {
    const details = new Map(output.center.map((notice) => [notice.seq, notice.detail]));
    const notices = output.lane.state.lane.flatMap((entry): [string, string][] => {
      if (entry.kind !== 'notice') {
        return [];
      }
      const detail = details.get(entry.seqs[entry.seqs.length - 1] ?? -1) ?? null;
      return detail === null ? [] : [[entry.id, detail]];
    });
    const byId = new Map(windows.map((entry) => [entry.windowId, entry]));
    const places = output.lane.visible.flatMap((item): [string, string][] => {
      const entry = item.kind === 'needs-you' ? byId.get(item.windowId) : undefined;
      return entry === undefined ? [] : [[item.id, roomNameOf(entry)]];
    });
    return new Map([...notices, ...places]);
  }

  /** A closed or timed-out pop-up; a window call stays away until its window arrives again. */
  function handleToastDone(item: LaneItem): void {
    if (item.kind === 'notice') {
      laneState = dismissLaneNotice(laneState, item.id);
      return;
    }
    const { windows } = get(registry);
    needsYouToasts = stepCallToasts(
      needsYouToasts,
      windows,
      currentWindowFor(workViewOf(get(shell)), windows),
      [],
      item.windowId
    );
  }

  function handleGoToWindowFromToast(windowId: string): void {
    const entry = get(registry).windows.find((candidate) => candidate.windowId === windowId);
    if (entry !== undefined) {
      goToWindowByUser(entry);
    }
  }

  function showWaitingWindows(): void {
    if (needsYouView === null) {
      toggleNeedsYouFilter();
    }
  }

  /** Opens or closes the Notifications list; opening marks everything listed so far as seen. */
  function toggleNoticeCenter(): void {
    if (isNoticeCenterOpen) {
      closeNoticeCenter('close-button');
      return;
    }
    noticeCenterAnchor = notificationsButton?.getBoundingClientRect() ?? null;
    noticeCenterNewerThan = seenThrough;
    seenThrough = Math.max(seenThrough, ...feed.center.map((notice) => notice.seq));
    isNoticeCenterOpen = true;
  }

  function openNoticeCenter(): void {
    if (!isNoticeCenterOpen) {
      toggleNoticeCenter();
    }
  }

  /**
   * Closes the Notifications list. Focus goes back to the bell when the list held it, except
   * after a press outside, which put focus where the user pressed.
   */
  function closeNoticeCenter(
    reason: OutsideDismissReason | 'escape' | 'tab' | 'close-button'
  ): void {
    const shouldReturnFocus =
      reason !== 'outside' && (panelHoldsFocus('.notice-center') || isFocusOnBody());
    isNoticeCenterOpen = false;
    if (shouldReturnFocus) {
      notificationsButton?.focus();
    }
  }

  // A modal makes the list behind it unusable; the modal takes focus itself.
  function closeNoticeCenterBehindModal(modals: number): void {
    if (modals > 0) {
      isNoticeCenterOpen = false;
    }
  }

  function dismissListedNotice(notice: Notice): void {
    if (notice.source === 'state') {
      noticeState = dismissNotice(noticeState, notice.key);
    } else {
      eventLog = dismissEvent(eventLog, notice.seq);
    }
  }

  function clearNoticeEvents(): void {
    eventLog = [];
  }

  /** Opens Settings on the Notifications tab, in place of the list. */
  function openNotificationSettings(): void {
    isNoticeCenterOpen = false;
    selectSettingsTab('notifications');
    if (isSettingsOpen) {
      pendingFocus.request({ kind: 'panel', panel: 'settings' });
    } else {
      void toggleSettings();
    }
  }

  function handleNoticeAction(action: NoticeAction): void {
    switch (action.labelKey) {
      case 'reconnect':
        void handleReconnect();
        return;
      case 'chooseWindows':
        openRestoreDialog();
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
   * answered by the next recovery status, which moves the strip within a tick.
   * The catch below guards the bridge itself, not the start.
   */
  async function handleReconnect(): Promise<void> {
    try {
      await invokeReconnectDaemon();
    } catch (error) {
      console.error('failed to trigger a manual daemon reconnect:', error);
    }
  }

  // Initialize on mount. The enumeration is chained behind the startup
  // sequence so it does not compete with it for the one control connection.
  void initialize().then(() => loadDetections());
  void loadKeymap();
  void loadKeymapFilePath();
  // After the themes file settles, so the daemon is never sent the state from
  // before it was read.
  let stopDefaultPaletteSync: (() => void) | null = null;
  void loadThemes().then(() => {
    stopDefaultPaletteSync = startDefaultPaletteSync(themeStore, ({ app, vendors }) =>
      invokeSetDefaultPalettes(app, vendors)
    );
  });
</script>

<svelte:window
  on:keydown|capture={handleWindowKeydown}
  on:resize={handleWindowResize}
  on:focus={() => (appFocused = true)}
  on:blur={() => (appFocused = false)}
/>
<svelte:document on:focusin={notePanelFocus} />

<main class="app">
  <NoticeAnnouncer announce={feed.announce} />

  <AlertStrip notices={feed.strip} onAction={handleNoticeAction} />

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
      windowsSummary={summarizeWindowStatuses($registry.windows)}
      onWindowsSelect={focusCurrentTerminal}
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
      notificationsUnseen={feed.unseen}
      isNotificationsOpen={isNoticeCenterOpen}
      onNotificationsToggle={toggleNoticeCenter}
      bind:notificationsButton
    />

    <NoticeCenter
      isOpen={isNoticeCenterOpen}
      anchor={noticeCenterAnchor}
      keepOpenWithin={notificationsButton}
      notices={feed.center}
      newerThan={noticeCenterNewerThan}
      onAction={handleNoticeAction}
      onDismiss={dismissListedNotice}
      onClear={clearNoticeEvents}
      onOpenSettings={openNotificationSettings}
      onClose={closeNoticeCenter}
    />

    <Navigator
      bind:element={sidebarElement}
      width={sidebarWidth}
      entries={$registry.windows}
      groups={sidebarGroupsNow}
      order={$registry.tabOrder}
      currentWindowId={current?.windowId ?? null}
      workMode={$shell.workMode}
      awaitingPermission={permissionWaiters}
      {needsYouView}
      {roomActions}
      {themeStore}
      onToggleNeedsYou={toggleNeedsYouFilter}
      onSelectWindow={goToWindowByUser}
      onChooseWorkMode={chooseWorkMode}
      {tileAvailability}
      onTileWindows={handleArrangeWindows}
      onNewWindow={openAddDialog}
      goToWindowChord={$keymapStore.keys['open-window-picker']}
      platform={keyboardPlatform}
      onGoToWindow={() => void runAction('open-window-picker')}
      onOpenChat={openChatForRoom}
      onRenameWindow={handleRenameWindow}
      onMoveWindow={registry.moveTab}
    />

    <!-- Pointer-only affordance: keyboard resize belongs to the shortcut
         layer, so the divider is not exposed to assistive technology. -->
    <div class="sidebar-resize" aria-hidden="true" use:pointerDrag={sidebarDrag}></div>

    <div class="work-surface">
      <div class="work-surface__windows">
        <WorkArea
          bind:element={mainContentElement}
          windows={$registry.windows}
          rendered={renderedPanes}
          {workView}
          currentWindowId={current?.windowId ?? null}
          {area}
          {layout}
          {actions}
          {themeStore}
          trackArea={trackFloatArea}
          onRename={handleRenameWindow}
          onPressed={(entry: WindowEntry, pressOnControl: boolean) =>
            activateWindow({ byUser: true, target: entry.windowId, pressOnControl })}
          exitedCard={$shell.exitedCard}
          keys={$keymapStore.keys}
          platform={keyboardPlatform}
          reopen={$registry.windows.length === 0 ? restoreOffer : null}
          onNewWindow={openAddDialog}
          onChooseWindows={openRestoreDialog}
          onExitedCardGoTo={() => dispatchShell({ type: 'exitedCardGoTo' })}
        />
      </div>
      <ToastRegion
        lane={feed.lane}
        secondLines={toastSecondLines}
        {timersRunning}
        onAction={handleNoticeAction}
        onGoToWindow={handleGoToWindowFromToast}
        onDismiss={handleToastDone}
        onExpire={handleToastDone}
        onShowWaiting={showWaitingWindows}
        onShowNotifications={openNoticeCenter}
        onReturnFocus={focusCurrentTerminal}
      />
    </div>
  </div>
</main>

{#if isChatOpen}
  <ChatPanel
    room={chatRoomStates.get(currentRoomId) ?? emptyChatRoom(currentRoomId)}
    rooms={[...chatRooms.values()]}
    {currentRoomId}
    onSelectRoom={selectChatRoom}
    onRefreshRooms={refreshChatRooms}
    onClearRoom={(room: ChatRoomInfo) => void removeChatRoom('clear_room', room)}
    onDeleteRoom={(room: ChatRoomInfo) => void removeChatRoom('delete_room', room)}
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
    activeTab={plansTab}
    plansAttention={plansAttention(planList)}
    onSelectTab={selectPlansTab}
    {ticketWorkspaces}
    selectedTicketWorkspaceId={ticketWorkspaceId}
    ticketMilestones={ticketOverview?.milestones ?? []}
    ticketMilestoneFilter={ticketMilestone}
    {ticketBoard}
    ticketConfig={ticketOverview?.config ?? null}
    {openTicketNumber}
    {openTicket}
    onSelectTicketWorkspace={selectTicketWorkspace}
    onSelectTicketMilestone={selectTicketMilestone}
    onOpenTicket={openTicketCard}
    onCloseTicket={closeTicket}
    onOpenTicketPlan={openTicketPlan}
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
    {notificationPreferences}
    onSetNotificationPreference={handleSetNotificationPreference}
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
  subject={confirmDialog.windowId}
  onConfirm={() => answerConfirm(true)}
  onCancel={() => answerConfirm(false)}
/>

<CommandPicker
  bind:this={commandPicker}
  isOpen={pickerMode !== null}
  mode={pickerMode ?? 'windows'}
  windows={pickerWindows}
  currentId={current?.windowId ?? null}
  awaitingPermission={permissionWaiters}
  keys={$keymapStore.keys}
  platform={keyboardPlatform}
  actionVerdict={availabilityOf}
  onChooseWindow={chooseWindowInPicker}
  onChooseAction={chooseActionInPicker}
  onCancel={cancelPicker}
/>

{#if approvalRequest !== null}
  <ApprovalDialog
    isOpen={isApprovalDialogOpen}
    request={approvalRequest}
    isRequesterOpen={isApprovalRequesterOpen}
    pendingCount={approvalQueue.size(approvals)}
    openedAt={approvalOpenedAt}
    decisionFailed={approvalQueue.decisionFailed(approvals, approvalRequest.request_id)}
    onDecide={decideApproval}
  />
{/if}

<RenameDialog
  bind:isOpen={renameDialog.isOpen}
  title={dialogs.renameTitle}
  label={forms.windowNameLabel}
  initialValue={renameDialog.entry?.name ?? ''}
  subject={renameDialog.entry?.windowId ?? null}
  onConfirm={async (name: string) => {
    const entry = renameDialog.entry;
    if (entry !== null) {
      await actions.rename(entry, name);
    }
  }}
  onCancel={() => {
    // Dialog will close automatically
  }}
/>

<Tooltip />

<!-- Where tooltips and menus are drawn while no modal dialog is open; one that is
     open has its own host inside it. -->
<div id="overlay-layer"></div>

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

  .layout {
    display: flex;
    flex: 1 1 auto;
    min-height: 0;
    gap: 0;
  }

  /* The positioned box the pop-ups sit in, at the work surface's bottom right. */
  .work-surface {
    position: relative;
    display: flex;
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
  }

  /* Window z-orders grow with the window count; kept inside their own stack so pop-ups stay above. */
  .work-surface__windows {
    display: flex;
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
    isolation: isolate;
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

  /* Above every panel; the elements drawn in it set their own order. */
  #overlay-layer {
    position: fixed;
    inset: 0;
    z-index: 100;
    pointer-events: none;
  }

  #overlay-layer > :global(*) {
    pointer-events: auto;
  }
</style>
