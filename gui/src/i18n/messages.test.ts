/**
 * Locale coverage is compiler-enforced: `Messages` is derived via `typeof`
 * from the English resource, so a locale missing a key is a type error rather
 * than a runtime surprise. The dummy locale lives here and is re-used by
 * `render.test.ts`; its `DUMMY_` prefix makes a leaked English string obvious
 * in a failing assertion.
 */
import { describe, it, expect } from 'vitest';

import { resolveMessages } from './messages';
import type { Messages } from './messages';

export const dummyMessages: Messages = {
  statusLabels: {
    running: 'DUMMY_running',
    waiting_input: 'DUMMY_waiting_input',
    waiting_approval: 'DUMMY_waiting_approval',
    interrupted: 'DUMMY_interrupted',
    idle: 'DUMMY_idle',
    exited: 'DUMMY_exited',
    unknown: 'DUMMY_unknown',
  },
  sessionPhaseLabels: {
    connecting: 'DUMMY_connecting',
    attached: 'DUMMY_attached',
    exited: 'DUMMY_phase_exited',
    superseded: 'DUMMY_superseded',
    failed: 'DUMMY_failed',
  },
  buttons: {
    close: 'DUMMY_close',
    interrupt: 'DUMMY_interrupt',
    rename: 'DUMMY_rename',
    submit: 'DUMMY_submit',
    cancel: 'DUMMY_cancel',
    minimize: 'DUMMY_minimize',
    maximize: 'DUMMY_maximize',
    restore: 'DUMMY_restore',
    chooseWindows: 'DUMMY_chooseWindows',
    selectAll: 'DUMMY_selectAll',
    clearAll: 'DUMMY_clearAll',
    // The approval dialog's two decision buttons.
    approve: 'DUMMY_approve',
    deny: 'DUMMY_deny',
    // The manual reconnect trigger on the recovery banner.
    reconnect: 'DUMMY_reconnect',
    howToInstall: 'DUMMY_howToInstall',
  },
  dialogs: {
    closeConfirm: 'DUMMY_closeConfirm',
    interruptConfirm: 'DUMMY_interruptConfirm',
    renameTitle: 'DUMMY_renameTitle',
    // The approval dialog's title and the two requester-display states
    // that are not the requester's own window.
    approvalTitle: 'DUMMY_approvalTitle',
    approvalRequesterExternal: 'DUMMY_approvalRequesterExternal',
    approvalRequesterGone: (name: string) => `DUMMY_approvalRequesterGone(${name})`,
    // Shown in place of a target window's name when a request carries none
    // (a chat-room-wide approval has no single target window).
    approvalTargetRoom: 'DUMMY_approvalTargetRoom',
    // Shown in place of a target window's name when the request is to open one.
    approvalTargetNewWindow: 'DUMMY_approvalTargetNewWindow',
    // Names the chat room a post would go to, when the request carries one.
    describeApprovalRoom: (name: string) => `DUMMY_describeApprovalRoom(${name})`,
  },
  forms: {
    commandLabel: 'DUMMY_commandLabel',
    cwdLabel: 'DUMMY_cwdLabel',
    windowNameLabel: 'DUMMY_windowNameLabel',
    commandPlaceholder: 'DUMMY_commandPlaceholder',
    cwdPlaceholder: 'DUMMY_cwdPlaceholder',
    commandHelp: 'DUMMY_commandHelp',
    nameLabel: 'DUMMY_nameLabel',
    namePlaceholder: 'DUMMY_namePlaceholder',
    detectionLabel: 'DUMMY_detectionLabel',
    detectionDefaultOption: 'DUMMY_detectionDefaultOption',
    addWindowTitle: 'DUMMY_addWindowTitle',
    browseButton: 'DUMMY_browseButton',
    folderDropHint: 'DUMMY_folderDropHint',
    closeDialog: 'DUMMY_closeDialog',
    terminalInput: 'DUMMY_terminalInput',
    optionalSuffix: 'DUMMY_optionalSuffix',
    // The launch-profile field and its read-only summary in
    // AddWindowDialog.svelte.
    profileLabel: 'DUMMY_profileLabel',
    profileUnavailableShort: {
      not_found: 'DUMMY_profileUnavailableShort_not_found',
      metacharacter: 'DUMMY_profileUnavailableShort_metacharacter',
      no_command: 'DUMMY_profileUnavailableShort_no_command',
      no_default_shell: 'DUMMY_profileUnavailableShort_no_default_shell',
      needs_native_program: 'DUMMY_profileUnavailableShort_needs_native_program',
    },
    customOption: 'DUMMY_customOption',
    profileSummaryCommandLabel: 'DUMMY_profileSummaryCommandLabel',
    profileSummaryCommandUnavailable: 'DUMMY_profileSummaryCommandUnavailable',
    profileSummaryDetectionLabel: 'DUMMY_profileSummaryDetectionLabel',
    // Launch-option fields, shown when the selected profile declares a
    // model / effort spec.
    modelLabel: 'DUMMY_modelLabel',
    effortLabel: 'DUMMY_effortLabel',
    modelLimited: 'DUMMY_modelLimited',
    effortLimited: 'DUMMY_effortLimited',
  },
  themes: {
    systemOption: 'DUMMY_systemOption',
    windowFollowsAppOption: 'DUMMY_windowFollowsAppOption',
    windowSelectLabel: 'DUMMY_windowSelectLabel',
    appTooltip: (name: string): string => `DUMMY_appTooltip ${name}`,
  },
  builtInThemeLabels: {
    dark: 'DUMMY_dark',
    light: 'DUMMY_light',
  },
  errors: {
    missingCommand: 'DUMMY_missingCommand',
    closeWindowFailed: 'DUMMY_closeWindowFailed',
    interruptWindowFailed: 'DUMMY_interruptWindowFailed',
    renameWindowFailed: 'DUMMY_renameWindowFailed',
    shortcutFailed: 'DUMMY_shortcutFailed',
    openWindowFailed: 'DUMMY_openWindowFailed',
    nameTakenOpenHint: 'DUMMY_nameTakenOpenHint',
    detectionLoadFailed: 'DUMMY_detectionLoadFailed',
    // One summary per boundary-failure call site, plus the prefix that
    // puts the count in front of a multi-diagnosis detail.
    boundarySessionEvent: 'DUMMY_boundarySessionEvent',
    boundarySessionEvents: 'DUMMY_boundarySessionEvents',
    boundaryWindowList: 'DUMMY_boundaryWindowList',
    boundaryChatChanged: 'DUMMY_boundaryChatChanged',
    boundaryThemes: 'DUMMY_boundaryThemes',
    boundaryDetectionsRefresh: 'DUMMY_boundaryDetectionsRefresh',
    boundaryDetectionsList: 'DUMMY_boundaryDetectionsList',
    boundaryProfilesIncomplete: 'DUMMY_boundaryProfilesIncomplete',
    boundaryProfilesList: 'DUMMY_boundaryProfilesList',
    boundaryDetailWithCount: (count: number, first: string) =>
      `DUMMY_boundaryDetailWithCount(${count},${first})`,
    // The launch-option-unsupported add-window-form message.
    launchOptionUnsupported: 'DUMMY_launchOptionUnsupported',
  },
  banners: {
    connectionFailed: 'DUMMY_connectionFailed',
    windowSuperseded: 'DUMMY_windowSuperseded',
    keymapLoadFailed: 'DUMMY_keymapLoadFailed',
    themesLoadFailed: 'DUMMY_themesLoadFailed',
    discardedKeymapEntries: (count: number) => `DUMMY_discardedKeymapEntries(${count})`,
    themeIssues: (count: number) => `DUMMY_themeIssues(${count})`,
    restoreAvailable: (count: number, timestamp: string | null) =>
      `DUMMY_restoreAvailable(${count},${String(timestamp)})`,
    restoreLoadFailed: 'DUMMY_restoreLoadFailed',
    daemonWindowsUnavailable: 'DUMMY_daemonWindowsUnavailable',
    // Unseen-and-expired approval requests.
    approvalExpiredUnseen: (
      count: number,
      latest: { requester: string; tool: string; target: string }
    ) =>
      `DUMMY_approvalExpiredUnseen(${count},${latest.requester},${latest.tool},${latest.target})`,
    // A failed decide_approval invocation, and a
    // register_approver rejection.
    approvalDecisionFailed: 'DUMMY_approvalDecisionFailed',
    approverRegistrationRefused: 'DUMMY_approverRegistrationRefused',
    // Daemon recovery and the client settings that drive it.
    daemonReconnecting: 'DUMMY_daemonReconnecting',
    daemonReconnectFailed: 'DUMMY_daemonReconnectFailed',
    clientConfigLoadFailed: 'DUMMY_clientConfigLoadFailed',
    // The daemon needs a restart before launch options work.
    daemonProtocolStale: 'DUMMY_daemonProtocolStale',
    profileNeedsNativeProgram: (names: readonly string[]) =>
      `DUMMY_profileNeedsNativeProgram(${names.join(',')})`,
    // The two reasons start-up could not resolve
    // this process's instance.
    invalidInstanceId: 'DUMMY_invalidInstanceId',
    storeOverrideWithNamedInstance: 'DUMMY_storeOverrideWithNamedInstance',
    restoreSkippedDisabledProfiles: (names: readonly string[]) =>
      `DUMMY_restoreSkippedDisabledProfiles(${names.join(',')})`,
  },
  hints: {
    windowList: 'DUMMY_windowList',
    noWindows: 'DUMMY_noWindows',
    interruptWindow: 'DUMMY_interruptWindow',
    closeWindow: 'DUMMY_closeWindow',
    minimizeWindow: 'DUMMY_minimizeWindow',
    maximizeWindow: 'DUMMY_maximizeWindow',
    restoreWindow: 'DUMMY_restoreWindow',
    focusWindow: 'DUMMY_focusWindow',
    dismissBanner: 'DUMMY_dismissBanner',
    reorderTab: 'DUMMY_reorderTab',
    tileWindows: 'DUMMY_tileWindows',
    restoreAlreadyOpen: 'DUMMY_restoreAlreadyOpen',
    restoreNoteExecution: 'DUMMY_restoreNoteExecution',
    restoreNoteUnselected: 'DUMMY_restoreNoteUnselected',
    // The room heading's collapse toggle and per-room action buttons,
    // plus the minimized-window marker shown in place of the cwd.
    collapseRoom: 'DUMMY_collapseRoom',
    expandRoom: 'DUMMY_expandRoom',
    tileRoom: 'DUMMY_tileRoom',
    minimizeRoom: 'DUMMY_minimizeRoom',
    restoreRoom: 'DUMMY_restoreRoom',
    minimizedMarker: 'DUMMY_minimizedMarker',
    // The hint on the header badge that jumps to the next window
    // needing the user.
    goToNextNeedingYou: 'DUMMY_goToNextNeedingYou',
    // Why the ⏹ button is disabled, one hint per blocked reason.
    // `renameWindow` is retired alongside it: the sidebar row drops the ✏️
    // button, and the window `⋯` menu already has its own label
    // (`menus.rename`).
    interruptUnavailableExited: 'DUMMY_interruptUnavailableExited',
    interruptUnavailableUnknown: 'DUMMY_interruptUnavailableUnknown',
    mcpConnected: 'DUMMY_mcpConnected',
    statusReportsMissing: 'DUMMY_statusReportsMissing',
  },
  formats: {
    exitCode: (code: number) => `DUMMY_exitCode(${code})`,
    windowListTitle: (label: string, count: number) => `DUMMY_windowListTitle(${label}, ${count})`,
    attentionBadge: (count: number) => `DUMMY_attentionBadge(${count})`,
    restoreSelectionCount: (selected: number, total: number) =>
      `DUMMY_restoreSelectionCount(${selected}, ${total})`,
    // The room heading's Unknown-count badge.
    unknownBadge: (count: number) => `DUMMY_unknownBadge(${count})`,
  },

  // Top-level rather than nested under `statusLabels`, so that group's own
  // equality contract below is not disturbed. `groups` (the state-based
  // group headings) is retired: rooms replace them.
  windowList: {
    shortStatus: {
      running: 'DUMMY_short_running',
      waiting_input: 'DUMMY_short_waiting_input',
      waiting_approval: 'DUMMY_short_waiting_approval',
      interrupted: 'DUMMY_short_interrupted',
      idle: 'DUMMY_short_idle',
      exited: 'DUMMY_short_exited',
      unknown: 'DUMMY_short_unknown',
    },
  },

  // Why a window's status is Unknown, in the three forms the sidebar
  // suffix, the title-bar/tooltip explanation, and the chat delivery chip
  // each need. Top-level (not nested under `statusLabels`) for the same
  // reason `windowList` is: it has its own key-set equality contract below.
  unknownReasons: {
    short: {
      no_definition: 'DUMMY_short_no_definition',
      no_rule_matched: 'DUMMY_short_no_rule_matched',
      screen_lost: 'DUMMY_short_screen_lost',
      detector_stopped: 'DUMMY_short_detector_stopped',
      not_yet_judged: 'DUMMY_short_not_yet_judged',
      declared_unknown: 'DUMMY_short_declared_unknown',
    },
    explanation: {
      no_definition: 'DUMMY_explanation_no_definition',
      no_rule_matched: 'DUMMY_explanation_no_rule_matched',
      screen_lost: 'DUMMY_explanation_screen_lost',
      detector_stopped: 'DUMMY_explanation_detector_stopped',
      not_yet_judged: 'DUMMY_explanation_not_yet_judged',
      declared_unknown: 'DUMMY_explanation_declared_unknown',
    },
    chatWord: {
      no_definition: 'DUMMY_chatWord_no_definition',
      no_rule_matched: 'DUMMY_chatWord_no_rule_matched',
      screen_lost: 'DUMMY_chatWord_screen_lost',
      detector_stopped: 'DUMMY_chatWord_detector_stopped',
      not_yet_judged: 'DUMMY_chatWord_not_yet_judged',
      declared_unknown: 'DUMMY_chatWord_declared_unknown',
    },
  },

  /** The `⋯` window menu's and the right-click menu's item labels. */
  menus: {
    theme: 'DUMMY_menu_theme',
    rename: 'DUMMY_menu_rename',
    copyOutput: 'DUMMY_menu_copyOutput',
    detectionDefinition: 'DUMMY_menu_detectionDefinition',
    sectionLabel: 'DUMMY_menu_sectionLabel',
    copy: 'DUMMY_menu_copy',
    paste: 'DUMMY_menu_paste',
  },

  /** Accessible names for the rail's icons and the `⋯` window-menu button. */
  settings: {
    panelTitle: 'DUMMY_settings_panelTitle',
    profilesTab: 'DUMMY_settings_profilesTab',
    profilesDescription: 'DUMMY_settings_profilesDescription',
    profileEnabled: 'DUMMY_settings_profileEnabled',
    profileDisabled: 'DUMMY_settings_profileDisabled',
    profileReady: 'DUMMY_settings_profileReady',
    profileNotFound: (reason: string) => `DUMMY_settings_profileNotFound(${reason})`,
    profileUnavailable: (reason: string) => `DUMMY_settings_profileUnavailable(${reason})`,
    profileUnavailableReason: {
      not_found: 'DUMMY_settings_profileUnavailableReason_not_found',
      metacharacter: 'DUMMY_settings_profileUnavailableReason_metacharacter',
      no_command: 'DUMMY_settings_profileUnavailableReason_no_command',
      no_default_shell: 'DUMMY_settings_profileUnavailableReason_no_default_shell',
      needs_native_program: 'DUMMY_settings_profileUnavailableReason_needs_native_program',
    },
    profileCopyCommand: 'DUMMY_settings_profileCopyCommand',
    profileCopyInstallGuide: 'DUMMY_settings_profileCopyInstallGuide',
    profileInstallGuide: 'DUMMY_settings_profileInstallGuide',
    profileSwitchLabel: (name: string) => `DUMMY_settings_profileSwitchLabel(${name})`,
    profileLimitsHeading: 'DUMMY_settings_profileLimitsHeading',
    profileLimitItem: (capability: string, reason: string) =>
      `DUMMY_settings_profileLimitItem(${capability},${reason})`,
    profileLimitGuide: 'DUMMY_settings_profileLimitGuide',
    profileCopyLimitGuide: 'DUMMY_settings_profileCopyLimitGuide',
    profileUsedByRoles: (roles: readonly string[]) =>
      `DUMMY_settings_profileUsedByRoles(${roles.join(',')})`,
    profileUpdateFailed: (id: string, reason: string, isEnabled: boolean) =>
      `DUMMY_settings_profileUpdateFailed(${id},${reason},${isEnabled})`,
    profileShowDetails: 'DUMMY_settings_profileShowDetails',
    profileHideDetails: 'DUMMY_settings_profileHideDetails',
    profileDetailLoading: 'DUMMY_settings_profileDetailLoading',
    profileDetailFailed: (reason: string) => `DUMMY_settings_profileDetailFailed(${reason})`,
    profileChanged: 'DUMMY_settings_profileChanged',
    profileShipped: 'DUMMY_settings_profileShipped',
    profileShippedValue: (value: string) => `DUMMY_settings_profileShippedValue(${value})`,
    profileCommandHeading: (os: string) => `DUMMY_settings_profileCommandHeading(${os})`,
    profileArgumentLabel: (index: number) => `DUMMY_settings_profileArgumentLabel(${index})`,
    profileRemoveArgument: (index: number) => `DUMMY_settings_profileRemoveArgument(${index})`,
    profileAddArgument: 'DUMMY_settings_profileAddArgument',
    profileSaveCommand: 'DUMMY_settings_profileSaveCommand',
    profileDiscardCommand: 'DUMMY_settings_profileDiscardCommand',
    profileResetCommand: (shipped: string) => `DUMMY_settings_profileResetCommand(${shipped})`,
    profileCommandFailed: (reason: string) => `DUMMY_settings_profileCommandFailed(${reason})`,
    profileAliasesHeading: 'DUMMY_settings_profileAliasesHeading',
    profileEditInModels: 'DUMMY_settings_profileEditInModels',
    profileLaunchHeading: 'DUMMY_settings_profileLaunchHeading',
    profileEnvironmentHeading: 'DUMMY_settings_profileEnvironmentHeading',
    profileShowValues: 'DUMMY_settings_profileShowValues',
    profileHideValues: 'DUMMY_settings_profileHideValues',
    profileProvidedHeading: 'DUMMY_settings_profileProvidedHeading',
    profileProvidedIncluded: 'DUMMY_settings_profileProvidedIncluded',
    profileProvidedLabel: (table: string) => `DUMMY_settings_profileProvidedLabel(${table})`,
    profileSettingsReadOnly: 'DUMMY_settings_profileSettingsReadOnly',

    approvalsTab: 'DUMMY_settings_approvalsTab',
    approvalsDescription: 'DUMMY_settings_approvalsDescription',
    approvalToolsHeading: 'DUMMY_settings_approvalToolsHeading',
    approvalPlansHeading: 'DUMMY_settings_approvalPlansHeading',
    approvalToolInfo: {
      open_window: {
        name: 'DUMMY_settings_approvalToolInfo_open_window_name',
        description: 'DUMMY_settings_approvalToolInfo_open_window_description',
        risk: 'DUMMY_settings_approvalToolInfo_open_window_risk',
      },
      send_chat: {
        name: 'DUMMY_settings_approvalToolInfo_send_chat_name',
        description: 'DUMMY_settings_approvalToolInfo_send_chat_description',
        risk: 'DUMMY_settings_approvalToolInfo_send_chat_risk',
      },
      broadcast_chat: {
        name: 'DUMMY_settings_approvalToolInfo_broadcast_chat_name',
        description: 'DUMMY_settings_approvalToolInfo_broadcast_chat_description',
        risk: 'DUMMY_settings_approvalToolInfo_broadcast_chat_risk',
      },
      send_message: {
        name: 'DUMMY_settings_approvalToolInfo_send_message_name',
        description: 'DUMMY_settings_approvalToolInfo_send_message_description',
        risk: 'DUMMY_settings_approvalToolInfo_send_message_risk',
      },
      send_handoff: {
        name: 'DUMMY_settings_approvalToolInfo_send_handoff_name',
        description: 'DUMMY_settings_approvalToolInfo_send_handoff_description',
        risk: 'DUMMY_settings_approvalToolInfo_send_handoff_risk',
      },
      send_keys: {
        name: 'DUMMY_settings_approvalToolInfo_send_keys_name',
        description: 'DUMMY_settings_approvalToolInfo_send_keys_description',
        risk: 'DUMMY_settings_approvalToolInfo_send_keys_risk',
      },
      interrupt_window: {
        name: 'DUMMY_settings_approvalToolInfo_interrupt_window_name',
        description: 'DUMMY_settings_approvalToolInfo_interrupt_window_description',
        risk: 'DUMMY_settings_approvalToolInfo_interrupt_window_risk',
      },
      close_window: {
        name: 'DUMMY_settings_approvalToolInfo_close_window_name',
        description: 'DUMMY_settings_approvalToolInfo_close_window_description',
        risk: 'DUMMY_settings_approvalToolInfo_close_window_risk',
      },
    },
    approvalToolSwitchLabel: (name: string) => `DUMMY_settings_approvalToolSwitchLabel(${name})`,
    planReviewSwitchLabel: 'DUMMY_settings_planReviewSwitchLabel',
    planReviewDescription: 'DUMMY_settings_planReviewDescription',
    approvalRunsAutomatically: 'DUMMY_settings_approvalRunsAutomatically',
    approvalWaitsForApproval: 'DUMMY_settings_approvalWaitsForApproval',
    planReviewFirst: 'DUMMY_settings_planReviewFirst',
    planRunsWithoutReview: 'DUMMY_settings_planRunsWithoutReview',
    planReviewOffRisk: 'DUMMY_settings_planReviewOffRisk',
    approvalSettingsNeedNewerDaemon: 'DUMMY_settings_approvalSettingsNeedNewerDaemon',
    approvalPlaybookFailed: 'DUMMY_settings_approvalPlaybookFailed',
    approvalPlaybookNotConfigured: 'DUMMY_settings_approvalPlaybookNotConfigured',
    approvalPlaybookUnknown: 'DUMMY_settings_approvalPlaybookUnknown',
    approvalToolUpdateFailure: (id: string, reason: string, wasOn: boolean) =>
      `DUMMY_settings_approvalToolUpdateFailure(${id},${reason},${wasOn})`,
    planUpdateFailure: (id: string, reason: string, wasOn: boolean) =>
      `DUMMY_settings_planUpdateFailure(${id},${reason},${wasOn})`,

    integrationsTab: 'DUMMY_settings_integrationsTab',
    integrationsDescription: 'DUMMY_settings_integrationsDescription',
    integrationsHostGroup: 'DUMMY_settings_integrationsHostGroup',
    integrationRegistered: 'DUMMY_settings_integrationRegistered',
    integrationNotRegistered: 'DUMMY_settings_integrationNotRegistered',
    integrationDiffers: 'DUMMY_settings_integrationDiffers',
    integrationNotInstalled: 'DUMMY_settings_integrationNotInstalled',
    integrationUnknown: 'DUMMY_settings_integrationUnknown',
    integrationChecking: 'DUMMY_settings_integrationChecking',
    integrationDiffersIn: (key: string) => `DUMMY_settings_integrationDiffersIn(${key})`,
    integrationRegisterButton: 'DUMMY_settings_integrationRegisterButton',
    integrationUpdateButton: 'DUMMY_settings_integrationUpdateButton',
    integrationRemoveButton: 'DUMMY_settings_integrationRemoveButton',
    integrationTomlSetNote: (
      file: string,
      table: readonly string[],
      values: readonly { key: string; value: string }[]
    ) =>
      `DUMMY_settings_integrationTomlSetNote(${file},${table.join('.')},${values
        .map((entry) => `${entry.key}=${entry.value}`)
        .join(',')})`,
    integrationFileWritesLead: 'DUMMY_settings_integrationFileWritesLead',
    integrationRefusedByClaim: (pointer: string) =>
      `DUMMY_settings_integrationRefusedByClaim(${pointer})`,
    integrationHookEntriesNote: (file: string, names: readonly string[]) =>
      `DUMMY_settings_integrationHookEntriesNote(${file},${names.join(',')})`,
    integrationsNoBridge: 'DUMMY_settings_integrationsNoBridge',
    integrationsNeedNewerDaemon: 'DUMMY_settings_integrationsNeedNewerDaemon',
    integrationsFooter: (bridge: string) => `DUMMY_settings_integrationsFooter(${bridge})`,
    integrationsListFailure: 'DUMMY_settings_integrationsListFailure',
    integrationRunFailure: (name: string) => `DUMMY_settings_integrationRunFailure(${name})`,

    keyboardTab: 'DUMMY_settings_keyboardTab',
    keyboardDescription: 'DUMMY_settings_keyboardDescription',
    keyboardChange: 'DUMMY_settings_keyboardChange',
    keyboardReset: 'DUMMY_settings_keyboardReset',
    keyboardRemove: 'DUMMY_settings_keyboardRemove',
    keyboardNotAssigned: 'DUMMY_settings_keyboardNotAssigned',
    keyboardChangedMark: 'DUMMY_settings_keyboardChangedMark',
    keyboardRecording: 'DUMMY_settings_keyboardRecording',
    keyboardGroupHeading: (group: string) => `DUMMY_settings_keyboardGroupHeading(${group})`,
    keyboardActionName: (action: string) => `DUMMY_settings_keyboardActionName(${action})`,
    keyboardActionNote: (action: string) => `DUMMY_settings_keyboardActionNote(${action})`,
    keyboardRejection: (reason: string, chordText: string) =>
      `DUMMY_settings_keyboardRejection(${reason},${chordText})`,
    keyboardTook: (chordText: string, fromName: string) =>
      `DUMMY_settings_keyboardTook(${chordText},${fromName})`,
    keyboardLost: (chordText: string) => `DUMMY_settings_keyboardLost(${chordText})`,
    keyboardSaveFailure: (kind: string) => `DUMMY_settings_keyboardSaveFailure(${kind})`,
    keyboardEditBlocked: (block: string, location: string) =>
      `DUMMY_settings_keyboardEditBlocked(${block},${location})`,
  },
  nav: {
    windows: 'DUMMY_nav_windows',
    // The Kanban seat becomes the Plans seat.
    plans: 'DUMMY_nav_plans',
    chat: 'DUMMY_nav_chat',
    theme: 'DUMMY_nav_theme',
    settings: 'DUMMY_nav_settings',
    windowMenu: 'DUMMY_nav_windowMenu',
    windowTabs: 'DUMMY_nav_windowTabs',
  },

  /** The chat panel, its delivery chips, its halt banner and its badge. */
  chat: {
    panelTitle: 'DUMMY_chat_panelTitle',
    closePanel: 'DUMMY_chat_closePanel',
    rosterLabel: 'DUMMY_chat_rosterLabel',
    rosterHeading: (count: number) => `DUMMY_chat_rosterHeading(${count})`,
    emptyMessages: 'DUMMY_chat_emptyMessages',
    inputPlaceholder: 'DUMMY_chat_inputPlaceholder',
    sendButton: 'DUMMY_chat_sendButton',
    broadcastButton: 'DUMMY_chat_broadcastButton',
    sendHint: 'DUMMY_chat_sendHint',
    haltedHint: 'DUMMY_chat_haltedHint',
    broadcastMarker: 'DUMMY_chat_broadcastMarker',
    needsYouBadge: 'DUMMY_chat_needsYouBadge',
    fromKindUser: 'DUMMY_chat_fromKindUser',
    fromKindWindow: 'DUMMY_chat_fromKindWindow',
    fromKindSystem: 'DUMMY_chat_fromKindSystem',
    fromKindExternal: 'DUMMY_chat_fromKindExternal',
    haltedRepetition: 'DUMMY_chat_haltedRepetition',
    haltedMonologue: 'DUMMY_chat_haltedMonologue',
    haltedTurnBudget: 'DUMMY_chat_haltedTurnBudget',
    haltedUnknown: 'DUMMY_chat_haltedUnknown',
    deliveryPending: 'DUMMY_chat_deliveryPending',
    deliveryTaken: 'DUMMY_chat_deliveryTaken',
    deliveryUndeliveredUnknown: 'DUMMY_chat_deliveryUndeliveredUnknown',
    // The reason-aware variant, shown when the daemon reported why
    // the addressed window's status is unknown.
    deliveryUndeliveredUnknownReason: (word: string) =>
      `DUMMY_chat_deliveryUndeliveredUnknownReason(${word})`,
    deliveryUndeliveredHalted: 'DUMMY_chat_deliveryUndeliveredHalted',
    deliveryUndeliveredExhausted: 'DUMMY_chat_deliveryUndeliveredExhausted',
    deliveryUndeliveredRestart: 'DUMMY_chat_deliveryUndeliveredRestart',
    deliveryUnknown: 'DUMMY_chat_deliveryUnknown',
    broadcastConfirmTitle: 'DUMMY_chat_broadcastConfirmTitle',
    broadcastConfirmMessage: 'DUMMY_chat_broadcastConfirmMessage',
    broadcastConfirmLabel: 'DUMMY_chat_broadcastConfirmLabel',
    // The room rail (left column of the two-column panel) and the
    // room-aware broadcast confirmation.
    roomsHeading: 'DUMMY_chat_roomsHeading',
    roomParticipantCount: (count: number) => `DUMMY_chat_roomParticipantCount(${count})`,
    roomStoppedLabel: 'DUMMY_chat_roomStoppedLabel',
    roomNeedsYouLabel: 'DUMMY_chat_roomNeedsYouLabel',
    broadcastConfirmRoomMessage: (name: string) =>
      `DUMMY_chat_broadcastConfirmRoomMessage(${name})`,
    roomActionsLabel: 'DUMMY_chat_roomActionsLabel',
    clearHistoryItem: 'DUMMY_chat_clearHistoryItem',
    deleteRoomItem: 'DUMMY_chat_deleteRoomItem',
    deleteMainRoomNote: 'DUMMY_chat_deleteMainRoomNote',
    deleteRoomWithWindowsNote: 'DUMMY_chat_deleteRoomWithWindowsNote',
    clearHistoryConfirmTitle: (name: string) => `DUMMY_chat_clearHistoryConfirmTitle(${name})`,
    clearHistoryConfirmMessage: 'DUMMY_chat_clearHistoryConfirmMessage',
    clearHistoryConfirmLabel: 'DUMMY_chat_clearHistoryConfirmLabel',
    deleteRoomConfirmTitle: (name: string) => `DUMMY_chat_deleteRoomConfirmTitle(${name})`,
    deleteRoomConfirmMessage: 'DUMMY_chat_deleteRoomConfirmMessage',
    deleteRoomConfirmLabel: 'DUMMY_chat_deleteRoomConfirmLabel',
    unresolvedMentions: (mentions: readonly string[]) =>
      `DUMMY_chat_unresolvedMentions(${mentions.join(',')})`,
  },

  /**
   * The Plans panel: its tabs, its board and roles controls, and the label
   * for each `AssignmentSource`. The four `source*` keys are
   * looked up dynamically through `assignmentSourceKey`
   * (`plans/plan-board-model.ts`), so their names are pinned here rather
   * than left to whichever identifier an implementation happens to pick.
   * Named as plain identifiers (not `assignment-source-*` string literals)
   * because the i18n source scan treats a key and a value as interchangeable
   * literals, and `plan-board-model.ts` holds the same strings as values.
   */
  plans: {
    ticketsTab: 'DUMMY_plans_ticketsTab',
    boardTab: 'DUMMY_plans_boardTab',
    rolesTab: 'DUMMY_plans_rolesTab',
    noOwnerBanner: 'DUMMY_plans_noOwnerBanner',
    assignOwnerButton: 'DUMMY_plans_assignOwnerButton',
    assignOwnerFailed: 'DUMMY_plans_assignOwnerFailed',
    applyAssignmentButton: 'DUMMY_plans_applyAssignmentButton',
    clearAssignmentButton: 'DUMMY_plans_clearAssignmentButton',
    deletePlanButton: 'DUMMY_plans_deletePlanButton',
    addRoleButton: 'DUMMY_plans_addRoleButton',
    removeRoleButton: 'DUMMY_plans_removeRoleButton',
    saveRolesButton: 'DUMMY_plans_saveRolesButton',
    sourceUser: 'DUMMY_plans_source_user',
    sourceParent: 'DUMMY_plans_source_parent',
    sourceRole: 'DUMMY_plans_source_role',
    sourceDefault: 'DUMMY_plans_source_default',
    sourceUnassigned: 'DUMMY_plans_source_unassigned',
    reportSucceeded: 'DUMMY_plans_reportSucceeded',
    reportFailed: 'DUMMY_plans_reportFailed',
    reportPathLabel: 'DUMMY_plans_reportPathLabel',
    reviewPendingTitle: 'DUMMY_plans_reviewPendingTitle',
    reviewPendingDetail: 'DUMMY_plans_reviewPendingDetail',
    reviewApprovedTitle: 'DUMMY_plans_reviewApprovedTitle',
    reviewApprovedDetail: 'DUMMY_plans_reviewApprovedDetail',
    reviewChangedTitle: 'DUMMY_plans_reviewChangedTitle',
    reviewChangedDetail: 'DUMMY_plans_reviewChangedDetail',
    approvePlanButton: 'DUMMY_plans_approvePlanButton',
    approvePlanBlockedHint: 'DUMMY_plans_approvePlanBlockedHint',
    discardAssignmentButton: 'DUMMY_plans_discardAssignmentButton',
    reviewMarkNew: 'DUMMY_plans_reviewMarkNew',
    reviewMarkChanged: 'DUMMY_plans_reviewMarkChanged',
    plansAttentionBadge: (isPending: boolean, count: number) =>
      `DUMMY_plans_plansAttentionBadge(${isPending},${count})`,
    defaultRoleLabel: 'DUMMY_plans_defaultRoleLabel',
    defaultRoleNone: 'DUMMY_plans_defaultRoleNone',
    defaultRoleHint: 'DUMMY_plans_defaultRoleHint',
    roleLaunchLimited: (profile: string, guidance: string | null) =>
      `DUMMY_plans_roleLaunchLimited(${profile},${guidance ?? ''})`,
    taskOpenLimited: (profile: string, guidance: string | null) =>
      `DUMMY_plans_taskOpenLimited(${profile},${guidance ?? ''})`,
    modelsTab: 'DUMMY_plans_modelsTab',
    modelsProfileLabel: 'DUMMY_plans_modelsProfileLabel',
    modelAliasColumn: 'DUMMY_plans_modelAliasColumn',
    modelValueColumn: 'DUMMY_plans_modelValueColumn',
    modelShippedColumn: 'DUMMY_plans_modelShippedColumn',
    modelChoicesFromProfile: 'DUMMY_plans_modelChoicesFromProfile',
    modelChoicesFromCommand: 'DUMMY_plans_modelChoicesFromCommand',
    modelChoicesFetchedAt: (time: string) => `DUMMY_plans_modelChoicesFetchedAt(${time})`,
    modelChoicesFetching: 'DUMMY_plans_modelChoicesFetching',
    modelChoicesFailed: (reason: string) => `DUMMY_plans_modelChoicesFailed(${reason})`,
    modelChoicesOverdueReason: 'DUMMY_plans_modelChoicesOverdueReason',
    modelOtherOption: 'DUMMY_plans_modelOtherOption',
    modelEditedMark: 'DUMMY_plans_modelEditedMark',
    saveModelAliasesButton: 'DUMMY_plans_saveModelAliasesButton',
    discardModelAliasesButton: 'DUMMY_plans_discardModelAliasesButton',
    refreshModelChoicesLabel: 'DUMMY_plans_refreshModelChoicesLabel',
    resetToShippedLabel: 'DUMMY_plans_resetToShippedLabel',
    modelForAliasLabel: (alias: string) => `DUMMY_plans_modelForAliasLabel(${alias})`,
  },
  tickets: {
    workspaceLabel: 'DUMMY_tickets_workspaceLabel',
    milestoneLabel: 'DUMMY_tickets_milestoneLabel',
    allMilestones: 'DUMMY_tickets_allMilestones',
    noWorkspaces: 'DUMMY_tickets_noWorkspaces',
    boardNote: (ticketCount: number, milestoneCount: number) =>
      `DUMMY_tickets_boardNote(${ticketCount},${milestoneCount})`,
    unconfigured: (reason: string) => `DUMMY_tickets_unconfigured(${reason})`,
    columnTruncated: 'DUMMY_tickets_columnTruncated',
    planCount: (count: number) => `DUMMY_tickets_planCount(${count})`,
    backButton: 'DUMMY_tickets_backButton',
    descriptionHeading: 'DUMMY_tickets_descriptionHeading',
    fieldsHeading: 'DUMMY_tickets_fieldsHeading',
    noRequiredFields: 'DUMMY_tickets_noRequiredFields',
    fieldNotSet: 'DUMMY_tickets_fieldNotSet',
    resultsHeading: 'DUMMY_tickets_resultsHeading',
    noResults: 'DUMMY_tickets_noResults',
    plansHeading: 'DUMMY_tickets_plansHeading',
    noPlans: 'DUMMY_tickets_noPlans',
  },
};

describe('unknownReasons', () => {
  it('unknownReasons.short/explanation/chatWord each cover exactly the six UnknownReason wire values', () => {
    const expected = [
      'no_definition',
      'no_rule_matched',
      'screen_lost',
      'detector_stopped',
      'not_yet_judged',
      'declared_unknown',
    ].sort();
    expect(Object.keys(resolveMessages().unknownReasons.short).sort()).toEqual(expected);
    expect(Object.keys(resolveMessages().unknownReasons.explanation).sort()).toEqual(expected);
    expect(Object.keys(resolveMessages().unknownReasons.chatWord).sort()).toEqual(expected);
  });
});
