/**
 * The consequence shared by every message that tells the user to restart the
 * daemon: restarting closes every window that is
 * currently open. Held as one constant so the sentence travels identically
 * wherever it is quoted, rather than being restated with drift.
 */
const RESTART_CLOSES_ALL_WINDOWS =
  'Restarting the daemon closes every window that is currently open.';

/**
 * The consequence lines `settings.approvalUpdateFailure` picks from, held as
 * constants so the switch/plan wording travels identically whether it is
 * quoted standalone (`settings.approvalStill*`) or inside that sentence.
 */
const APPROVAL_STILL_RUNS_AUTOMATICALLY = 'It still runs automatically.';
const APPROVAL_STILL_WAITS_FOR_APPROVAL = 'It still waits for your approval.';
const PLAN_STILL_REVIEWS_FIRST = 'It still reviews new plans before they run.';
const PLAN_STILL_RUNS_WITHOUT_REVIEW = 'It still runs new plans without review.';

/** `focusNextWindow` -> `focus-next-window`, at the type level. */
type KebabCase<S extends string> = S extends `${infer Head}${infer Tail}`
  ? `${Head extends Lowercase<Head> ? Head : `-${Lowercase<Head>}`}${KebabCase<Tail>}`
  : S;

/**
 * The Keyboard tab tables are keyed by the camelCase form of an id, so this
 * file holds no quoted copy of an action, group or refusal id. The
 * parameter types of the functions below are the ids those keys spell, so a
 * caller holding an id the table lacks fails to compile.
 */
function camelCase(id: string): string {
  return id.replace(/-([a-z])/g, (_match, letter: string) => letter.toUpperCase());
}

/** Looks `id` up in a camelCase-keyed table. */
function lookup<T>(table: Readonly<Record<string, T>>, id: string): T {
  return table[camelCase(id)] as T;
}

const KEYBOARD_GROUP_HEADINGS = {
  windows: 'Windows',
  focusAndPanels: 'Focus and panels',
};

const KEYBOARD_ACTION_NAMES = {
  focusNextWindow: 'Next window',
  focusPreviousWindow: 'Previous window',
  focusNextAttention: 'Next window needing you',
  openWindowForm: 'New window',
  closeWindow: 'Close window',
  interruptWindow: 'Interrupt window',
  minimizeWindow: 'Minimize window',
  toggleMaximizeWindow: 'Maximize or restore window',
  toggleSidebarFocus: 'Move to sidebar / back to window',
  toggleChat: 'Chat panel',
  togglePlans: 'Plans panel',
  toggleSettings: 'Settings panel',
};

/** The one row that says more than its name: the next-window order follows the sidebar. */
const KEYBOARD_ACTION_NOTES: Readonly<Record<string, string>> = {
  focusNextWindow: 'In sidebar order',
};

const KEYBOARD_REJECTIONS = {
  invalidChord: (chord: string) =>
    `Not saved: ${chord} is not a key combination yagura understands.`,
  needsModifier: () =>
    'Not saved: add Ctrl (or Cmd on macOS). A key without a modifier would be taken away from the programs in your windows.',
  altReachesTerminal: (chord: string) =>
    `Not saved: ${chord} uses Alt or Option, which the programs in your windows use. Use Ctrl+Shift (Cmd on macOS) instead.`,
  typesCharacters: (chord: string) =>
    `Not saved: ${chord} types characters on some keyboard layouts, so it is not accepted.`,
  reservedForClipboard: (chord: string) => `Not saved: ${chord} is kept for copy and paste.`,
  reservedByAppMenu: (chord: string) => `Not saved: ${chord} belongs to the app menu.`,
  reachesTerminal: (chord: string) =>
    `Not saved: ${chord} reaches the shell or CLI inside your windows (for example, a shell searches its history with it). Add Shift or use another key.`,
  reservedForReload: (chord: string) =>
    `Not saved: ${chord} would reload yagura, so it is kept switched off.`,
};

const KEYBOARD_SAVE_FAILURES = {
  no_config_root: 'Not saved: this instance has no place to keep keymap.toml.',
  too_large: 'Not saved: the keymap would be too large to load back.',
  write_failed: 'Not saved: keymap.toml could not be written.',
  unexpected: 'Not saved: something unexpected went wrong while saving.',
};

const KEYBOARD_EDIT_BLOCKS = {
  not_loaded: (location: string) =>
    `The keyboard settings are still loading. Changes are possible once ${location} has been read.`,
  rejected: (location: string) =>
    `yagura could not read ${location}, so keyboard changes are turned off to avoid overwriting it.`,
  read_failed: (location: string) =>
    `${location} could not be read, so keyboard changes are turned off to avoid overwriting it.`,
  invalid_toml: (location: string) =>
    `${location} is not valid TOML, so keyboard changes are turned off. Fix or delete the file, then restart yagura.`,
  too_large: (location: string) =>
    `${location} is too large to read, so keyboard changes are turned off. Make it smaller, then restart yagura.`,
};

/**
 * English display strings — the only place display text lives.
 *
 * No type annotation on this object: `Messages` (`./messages.ts`) derives its
 * type via `typeof`, so a second locale missing a key is a compile error
 * there rather than a runtime fallback here.
 */
export const messagesEn = {
  statusLabels: {
    running: 'Running',
    waiting_input: 'Waiting for input',
    waiting_approval: 'Waiting for approval',
    interrupted: 'Interrupted',
    idle: 'Idle',
    exited: 'Exited',
    unknown: 'Unknown',
  },

  sessionPhaseLabels: {
    connecting: 'Connecting…',
    attached: 'Attached',
    exited: 'The process has exited',
    superseded: 'Another window connected',
    failed: 'Cannot reach the daemon',
  },

  buttons: {
    close: 'Close',
    interrupt: 'Interrupt',
    rename: 'Rename',
    submit: 'Add',
    cancel: 'Cancel',
    minimize: 'Minimize',
    maximize: 'Maximize',
    restore: 'Restore',
    chooseWindows: 'Choose windows…',
    selectAll: 'Select all',
    clearAll: 'Clear all',
    approve: 'Approve',
    deny: 'Deny',
    reconnect: 'Reconnect',
    /** Opens a profile's row in Settings, where its install guidance is shown. */
    howToInstall: 'How to install',
  },

  dialogs: {
    closeConfirm: 'This window will be closed. Continue?',
    interruptConfirm: 'This process will be interrupted. Continue?',
    renameTitle: 'Rename window',
    approvalTitle: 'Approval required',
    approvalRequesterExternal: 'External client',

    /** Shown when the requesting window is no longer in the window list. */
    approvalRequesterGone: (name: string): string => `Window ${name} (closed)`,

    /** Stands in for a target window when the request names no room either. */
    approvalTargetRoom: 'the chat room',

    /** Stands in for a target window when the request is to open one that does not exist yet. */
    approvalTargetNewWindow: 'a new window',

    /** Names the chat room a request is addressed to, when it names one. */
    describeApprovalRoom: (name: string): string => `the chat room “${name}”`,
  },

  forms: {
    commandLabel: 'Command (one token per line)',
    cwdLabel: 'Working directory',
    windowNameLabel: 'Enter a window name',
    commandPlaceholder: 'line 1: python\nline 2: script.py\nline 3: --option',
    cwdPlaceholder: 'e.g. /home/user/project',
    commandHelp: 'Each line is treated as one token, so paths containing spaces work as well.',
    nameLabel: 'Name',
    namePlaceholder: 'e.g. builder',
    detectionLabel: 'Detection definition',
    detectionDefaultOption: 'Use the default detection definition',
    profileLabel: 'Launch profile',
    /** Why a profile cannot be chosen, short enough to follow its name in a select. */
    profileUnavailableShort: {
      not_found: 'not found',
      metacharacter: "command can't be passed safely",
      no_command: 'no command for this OS',
      no_default_shell: 'no default shell',
      needs_native_program: 'needs a different install (see Settings › Profiles)',
    },
    customOption: 'Custom…',
    profileSummaryCommandLabel: 'Command:',
    profileSummaryCommandUnavailable: 'Provided by the daemon at launch',
    profileSummaryDetectionLabel: 'Detection definition:',
    addWindowTitle: 'Add a window',
    browseButton: 'Browse…',
    folderDropHint: 'Drop a folder here',
    closeDialog: 'Close dialog',
    terminalInput: 'Terminal input',
    optionalSuffix: 'optional',
    modelLabel: 'Model',
    effortLabel: 'Effort',
  },

  themes: {
    appLabel: 'Theme',
    systemOption: 'System',
    windowFollowsAppOption: 'Same as app',
    windowSelectLabel: 'Theme for this window',
    appTooltip: (name: string): string => `Theme: ${name}`,
  },

  /**
   * Display names of the built-in themes, kept as an object independent of
   * `themes`: `themes` is re-exported by the facade, and nesting this under
   * it would put it on the public surface even though the facade does not
   * intend to re-publish it.
   */
  builtInThemeLabels: {
    dark: 'Dark',
    light: 'Light',
  },

  errors: {
    missingCommand: 'A command is required',
    invalidCommand: 'The command has to be non-empty text',
    closeWindowFailed: 'Could not close the window',
    interruptWindowFailed: 'Could not interrupt the process',
    renameWindowFailed: 'Could not rename the window',
    shortcutFailed: 'Could not carry out the keyboard shortcut',
    openWindowFailed: 'Could not open the window',
    detectionLoadFailed: 'Could not load the detection definition',

    // One summary per boundary-failure call site. Each names only the
    // operation that could not be carried out, so it stays true whatever the
    // cause turned out to be; the cause itself arrives as the detail. The
    // three that end in "so the view may be behind" are the ones where the
    // WebView itself knows it dropped a payload.
    boundarySessionEvent: 'Could not apply a session event, so the view may be behind',
    boundarySessionEvents: "Could not subscribe to the daemon's events",
    boundaryWindowList: 'Could not refresh the window list, so the view may be behind',
    boundaryChatChanged: 'Could not apply a chat update, so the view may be behind',
    boundaryThemes: 'Could not load the theme settings',
    boundaryDetectionsRefresh: 'Could not refresh the detection definitions',
    boundaryDetectionsList: 'Could not list the detection definitions',
    boundaryProfilesIncomplete: 'Not all launch profiles could be read',
    boundaryProfilesList: 'Could not list the launch profiles',

    /**
     * Puts the count in front of the first diagnosis when a boundary failure
     * carries several. Leading rather than trailing: the detail is capped at
     * its tail, so a trailing count would be the first thing cut off.
     */
    boundaryDetailWithCount: (count: number, first: string): string =>
      `${count} problems, first: ${first}`,

    // Shown by the add-window form when the connected
    // daemon has not settled a wire revision the requested launch options
    // need. Never names the wire tag of the refused request -- that value is
    // machine-readable, not something to show the user.
    launchOptionUnsupported: `This launch option needs a newer daemon. ${RESTART_CLOSES_ALL_WINDOWS}`,
  },

  banners: {
    connectionFailed: 'Cannot reach the daemon',
    windowSuperseded: 'Another client is operating this window',
    keymapLoadFailed: 'Could not load the keymap. Falling back to the default key bindings.',
    themesLoadFailed: 'Could not load the theme settings. Falling back to the built-in themes.',

    /** Plural form policy lives with the resource, not the machinery. */
    discardedKeymapEntries: (count: number): string =>
      count === 1
        ? 'Discarded 1 invalid keymap entry. That key falls back to its default binding.'
        : `Discarded ${count} invalid keymap entries. Those keys fall back to their default bindings.`,

    themeIssues: (count: number): string =>
      count === 1
        ? 'Excluded 1 invalid theme entry. That theme cannot be selected.'
        : `Excluded ${count} invalid theme entries. Those themes cannot be selected.`,

    restoreAvailable: (count: number, timestamp: string | null): string => {
      const subject = count === 1 ? '1 window' : `${count} windows`;
      const verb = count === 1 ? 'is' : 'are';
      const base = `${subject} from your last session ${verb} available to restore`;
      return timestamp === null ? `${base}.` : `${base} (saved ${timestamp}).`;
    },
    restoreLoadFailed: 'Previous session could not be read.',
    daemonWindowsUnavailable:
      'Windows cannot be opened: the console host bundled with yagura (conpty.dll, OpenConsole.exe) is missing or damaged. Reinstall yagura.',

    /** Plural form policy lives with the resource, not the machinery. */
    approvalExpiredUnseen: (
      count: number,
      latest: { requester: string; tool: string; target: string }
    ): string => {
      const subject =
        count === 1 ? '1 approval request expired' : `${count} approval requests expired`;
      return `${subject} before you could answer. Most recent: ${latest.tool} on ${latest.target}, asked for by ${latest.requester}.`;
    },

    approvalDecisionFailed: 'Could not send your decision to the daemon. Please try again.',
    approverRegistrationRefused: `Approvals are unavailable: restart the daemon (yagura-daemon --shutdown). ${RESTART_CLOSES_ALL_WINDOWS}`,

    // What daemon recovery reports, and why the client settings that
    // drive it were not used.
    daemonReconnecting: 'Lost the connection to the daemon. Reconnecting…',
    daemonReconnectFailed: 'Could not reconnect to the daemon after several attempts.',
    clientConfigLoadFailed:
      'Could not load the client settings. Falling back to the default reconnection settings.',

    // The connected daemon has not settled a
    // wire revision that can show a window (attach_at). Reported
    // separately from approverRegistrationRefused because it is a fact about
    // the wire revision, not about approver registration.
    daemonProtocolStale: `This daemon is too old to show windows: restart it (yagura-daemon --shutdown). ${RESTART_CLOSES_ALL_WINDOWS}`,

    /** Enabled profiles whose program is installed as a Windows .cmd / .bat file, which they cannot run through. */
    profileNeedsNativeProgram: (names: readonly string[]): string =>
      names.length === 1
        ? `${names[0] ?? ''} can't be used: it's installed as a Windows .cmd / .bat file.`
        : `${names.join(', ')} can't be used: they're installed as Windows .cmd / .bat files.`,

    // Start-up could not work out which instance
    // this window belongs to, so no daemon was started or connected. The
    // rejected identifier comes from the environment and is never quoted
    // back here; the rules it broke are stated instead.
    invalidInstanceId:
      'YAGURA_INSTANCE is not a usable instance name, so yagura started without a daemon. An instance name is 1 to 16 characters of lowercase letters, digits and hyphens, and begins and ends with a letter or a digit. Correct the value, or unset YAGURA_INSTANCE to use the default instance, then start yagura again.',
    storeOverrideWithNamedInstance:
      'A named instance cannot run while YAGURA_STORE_DB is set, so yagura started without a daemon: both instances would open the same database, and whichever started last would discard the previous session saved by the other. Unset YAGURA_STORE_DB in this terminal and start yagura again.',

    // Restore leaves out the windows whose launch profile is switched off,
    // rather than starting the CLI from the saved command line.
    restoreSkippedDisabledProfiles: (names: readonly string[]): string =>
      names.length === 1
        ? `Not restored because its launch profile is disabled: ${names.join(', ')}`
        : `Not restored because their launch profiles are disabled: ${names.join(', ')}`,
  },

  hints: {
    windowList: 'Open windows',
    noWindows: 'No windows are open yet',
    interruptWindow: 'Interrupt the process',
    closeWindow: 'Close the window',
    minimizeWindow: 'Minimize the window',
    maximizeWindow: 'Maximize the window',
    restoreWindow: 'Restore the window to its previous size',
    focusWindow: 'Click to bring to front',
    dismissBanner: 'Dismiss this notice',
    reorderTab: 'Drag to reorder',
    tileWindows: 'Tile windows',
    restoreAlreadyOpen: 'Already open',
    restoreNoteExecution: 'Running state and unsaved output are not restored.',
    restoreNoteUnselected:
      "Windows you don't restore now won't be offered again after the daemon's next restart.",

    // The room heading's collapse toggle and per-room action buttons,
    // plus the minimized-window marker shown in place of the cwd.
    collapseRoom: 'Collapse this room',
    expandRoom: 'Expand this room',
    tileRoom: "Tile this room's windows",
    minimizeRoom: "Minimize this room's windows",
    restoreRoom: "Restore this room's windows",
    minimizedMarker: 'Minimized',

    // The header badge doubles as the way to reach the next window
    // needing the user.
    goToNextNeedingYou: 'Go to the next window that needs you',

    // Why the interrupt button is disabled. The two reasons are
    // separate facts -- nothing is left to interrupt, versus nothing is known
    // about what the window is doing -- so they stay separate sentences.
    interruptUnavailableExited: 'Cannot interrupt: the process has already exited',
    interruptUnavailableUnknown: "Cannot interrupt: this window's state is unknown",

    // The mark shown next to a window whose child is connected to
    // yagura over MCP.
    mcpConnected: 'Connected to yagura',

    // The mark shown next to a window whose CLI was given hooks but whose
    // status reports are not arriving.
    statusReportsMissing:
      "Status reports from this CLI aren't arriving. yagura is reading the screen only.",
  },

  formats: {
    exitCode: (code: number): string => `(exit code ${code})`,
    windowListTitle: (label: string, count: number): string => `${label} (${count})`,

    /** Plural form policy lives with the resource, not the machinery. */
    attentionBadge: (count: number): string =>
      count === 1 ? '1 window needs you' : `${count} windows need you`,

    restoreSelectionCount: (selected: number, total: number): string =>
      `${selected} of ${total} selected`,

    /** The room heading's Unknown-count badge. */
    unknownBadge: (count: number): string =>
      count === 1 ? '1 window in an unknown state' : `${count} windows in an unknown state`,
  },

  /**
   * A top-level group rather than a nesting under `statusLabels`, so that
   * group's equality contract is not disturbed. The state-based group
   * headings (`groups`) were retired: rooms replace them.
   */
  windowList: {
    shortStatus: {
      running: 'Running',
      waiting_input: 'Needs input',
      waiting_approval: 'Needs approval',
      interrupted: 'Interrupted',
      idle: 'Idle',
      exited: 'Exited',
      unknown: 'Unknown',
    },
  },

  /**
   * Why a window's status is Unknown, in the three forms the sidebar suffix,
   * the title-bar/tooltip explanation, and the chat delivery chip each need.
   * Top-level rather than nested under `statusLabels`, the same
   * reason `windowList` is.
   */
  unknownReasons: {
    short: {
      no_definition: 'No definition',
      no_rule_matched: 'No rule matched',
      screen_lost: 'Screen lost',
      detector_stopped: 'Detector stopped',
      not_yet_judged: 'Not judged yet',
      declared_unknown: 'Reported unknown',
    },
    explanation: {
      no_definition:
        "No usable detection definition is assigned, so the status can never be detected. Assign one with open_window's detection argument or fix the profile.",
      no_rule_matched:
        'The screen matches no rule of the detection definition. Look at the window; messages stay refused until a rule matches or the definition settles this window on time alone.',
      screen_lost:
        'The screen model was lost and has not been trusted again. Look at the window; the status recovers once the window settles on a state that accepts input.',
      detector_stopped:
        'Detection stopped reporting for this window. Close the window and open it again.',
      not_yet_judged: 'The window just opened and nothing has been judged yet. Wait for output.',
      declared_unknown:
        'A detection rule or a status event reported the status as unknown. Look at the window.',
    },
    chatWord: {
      no_definition: 'no definition',
      no_rule_matched: 'no rule matched',
      screen_lost: 'screen lost',
      detector_stopped: 'detector stopped',
      not_yet_judged: 'not judged yet',
      declared_unknown: 'reported unknown',
    },
  },

  menus: {
    theme: 'Theme',
    rename: 'Rename…',
    copyOutput: 'Copy output',
    detectionDefinition: 'Detection definition',
    sectionLabel: 'This window',
    copy: 'Copy',
    paste: 'Paste',
  },

  /**
   * The chat panel, its delivery chips, its halt banner and its badge. A
   * group of its own so the confirmation wording the panel needs does not
   * enter `buttons` / `dialogs`, whose key sets are frozen elsewhere.
   */
  chat: {
    panelTitle: 'Chat',
    closePanel: 'Close chat',
    rosterLabel: 'In this room',
    rosterHeading: (count: number): string => `In this room (${count})`,
    emptyMessages: 'No messages in this room yet.',
    inputPlaceholder: 'Write to the room. Type @ to pick a participant.',
    sendButton: 'Send',
    broadcastButton: 'To everyone',
    sendHint: 'Enter sends, Shift+Enter adds a line',
    haltedHint: 'Your message clears the halt',
    broadcastMarker: 'To everyone',
    needsYouBadge: 'The chat room needs you',

    fromKindUser: 'You',
    fromKindWindow: 'Window',
    fromKindSystem: 'System',
    fromKindExternal: 'External',

    haltedRepetition:
      'The room stopped: repeated exchange. The same message came back too many times. Write something to start it again.',
    haltedMonologue:
      'The room stopped: one voice only. One participant spoke several times with no one answering. Write something to start it again.',
    haltedTurnBudget:
      'The room stopped: turn budget spent. The participants used up the messages allowed since your last one. Write something to start it again.',
    haltedUnknown: 'The room stopped. Write something to start it again.',

    /** The post waits in the window's inbox until its CLI takes it, which can be at its next turn. */
    deliveryPending: 'Queued',
    deliveryTaken: 'Delivered',
    deliveryUndeliveredUnknown: 'Not sent (status unknown)',
    /** The reason-aware variant, shown when the daemon reported why the addressed window's status is unknown. */
    deliveryUndeliveredUnknownReason: (word: string): string =>
      `Not sent (status unknown: ${word})`,
    deliveryUndeliveredHalted: 'Not sent (room halted)',
    deliveryUndeliveredExhausted: 'Not sent (too many deliveries)',
    deliveryUndeliveredRestart: 'Not sent (daemon restarted)',
    deliveryUnknown: 'Unknown delivery state',

    broadcastConfirmTitle: 'Send to everyone?',
    broadcastConfirmMessage: 'This message goes to every participant in the room. Continue?',
    broadcastConfirmLabel: 'Broadcast',

    // The room rail (left column of the two-column panel) and the
    // room-aware broadcast confirmation.
    roomsHeading: 'Rooms',
    roomParticipantCount: (count: number): string =>
      count === 0 ? 'no windows' : count === 1 ? '1 participant' : `${count} participants`,
    roomStoppedLabel: 'Room stopped',
    roomNeedsYouLabel: 'This room needs you',
    broadcastConfirmRoomMessage: (name: string): string =>
      `This message goes to every participant in the chat room “${name}”. Continue?`,
  },

  /**
   * The plans panel: its tabs, the board's controls and owner banner, the
   * role table editor, and one label per assignment source. The source keys
   * are the ones `assignmentSourceKey` returns, looked up by that value.
   */
  plans: {
    boardTab: 'Board',
    rolesTab: 'Roles',
    noOwnerBanner:
      'No open window owns this plan, so no agent can update it. Assign a window to take it over.',
    assignOwnerButton: 'Assign owner',
    assignOwnerFailed: 'Could not assign that window as the owner. It may have closed.',
    applyAssignmentButton: 'Apply',
    clearAssignmentButton: 'Reset to default',
    deletePlanButton: 'Delete plan',
    addRoleButton: 'Add role',
    removeRoleButton: 'Remove role',
    saveRolesButton: 'Save roles',
    sourceUser: 'Set by you',
    sourceParent: 'Set by the plan',
    sourceRole: 'From the role',
    sourceDefault: 'From the default role',
    sourceUnassigned: 'Unassigned',
    reportSucceeded: 'Succeeded',
    reportFailed: 'Failed',
    reportPathLabel: 'Result file',

    // The review banner under the plan picker and the tags on tasks added or
    // changed after approval.
    reviewPendingTitle: 'Waiting for your review.',
    reviewPendingDetail:
      "Check each task's assignment. No task window can open until you approve this plan.",
    reviewApprovedTitle: 'Approved.',
    reviewApprovedDetail:
      "Task windows can open. If the plan's owner adds or changes tasks later, they still open and you are notified here.",
    reviewChangedTitle: 'Changed since you approved it.',
    reviewChangedDetail:
      "The highlighted tasks were added or changed by the plan's owner. They open as planned; change an assignment here before its task opens if you want a different one.",
    approvePlanButton: 'Approve plan',
    approvePlanBlockedHint: 'Apply or discard your assignment changes before approving the plan.',
    discardAssignmentButton: 'Discard',
    reviewMarkNew: 'New',
    reviewMarkChanged: 'Changed',

    // The sidebar's Plans badge tooltip: which kind of attention (pending
    // review, or changed since approval) and how many plans, pluralized.
    plansAttentionBadge: (isPending: boolean, count: number): string =>
      isPending
        ? count === 1
          ? '1 plan is waiting for your review'
          : `${count} plans are waiting for your review`
        : count === 1
          ? '1 plan changed since you approved it'
          : `${count} plans changed since you approved them`,

    // The role table's default role picker; the first option picks none.
    defaultRoleLabel: 'Default role',
    defaultRoleNone: 'None',
    defaultRoleHint: 'Used for any task whose role is missing or not in this table.',

    // The Models tab: the model each alias of a launch profile stands for.
    modelsTab: 'Models',
    modelsProfileLabel: 'Profile',
    modelAliasColumn: 'Alias',
    modelValueColumn: 'Model',
    modelShippedColumn: 'Shipped',
    modelChoicesFromProfile: 'Choices from this profile',
    modelChoicesFromCommand: 'Models from',
    modelChoicesFetchedAt: (time: string): string => `· fetched ${time}`,
    modelChoicesFetching: 'Fetching…',
    modelChoicesFailed: (reason: string): string =>
      `Could not read the model list: ${reason}. You can still type model names.`,
    modelChoicesOverdueReason: 'the request timed out',
    modelOtherOption: 'Other…',
    modelEditedMark: 'edited',
    saveModelAliasesButton: 'Save',
    discardModelAliasesButton: 'Discard',
    refreshModelChoicesLabel: 'Refresh the model list',
    resetToShippedLabel: 'Reset to the shipped value',
    modelForAliasLabel: (alias: string): string => `Model for ${alias}`,
  },

  settings: {
    panelTitle: 'Settings',
    profilesTab: 'Profiles',
    profilesDescription:
      'Choose which launch profiles yagura uses, and see or change what each one runs. A disabled profile is left out of every profile list, and neither you nor an agent can open a window from it. Your changes are kept in your own file; everything else follows the version of yagura you run.',
    profileEnabled: 'Enabled',
    profileDisabled: 'Disabled',
    profileReady: 'Ready',
    profileNotFound: (reason: string): string => `Not found — ${reason}`,
    profileUnavailable: (reason: string): string => `Unavailable — ${reason}`,
    /** Why a profile cannot be launched, after the row's "Not found" / "Unavailable" heading. */
    profileUnavailableReason: {
      not_found: 'the program this profile runs could not be found',
      metacharacter:
        'the command line contains a character the Windows command interpreter would change',
      no_command: 'no command is set for this operating system',
      no_default_shell: 'no default shell was found when the daemon started',
      needs_native_program:
        "yagura passes messages to this CLI as arguments, which a Windows .cmd / .bat file can't carry unchanged",
    },
    /** The visible text is the accessible name, so the two buttons differ for a screen reader too. */
    profileCopyCommand: 'Copy command',
    profileCopyInstallGuide: 'Copy install guide address',
    /** Labels the page a profile's guidance points to; shown as text to copy, not a link. */
    profileInstallGuide: 'Install guide',
    profileSwitchLabel: (name: string): string => `Use ${name}`,

    /** List and plural form policy lives with the resource, not the machinery. */
    profileUsedByRoles: (roles: readonly string[]): string => {
      const last = roles[roles.length - 1] ?? '';
      const listed =
        roles.length <= 2 ? roles.join(' and ') : `${roles.slice(0, -1).join(', ')}, and ${last}`;
      return roles.length === 1
        ? `Used by the role ${listed}. Its tasks cannot open windows while this profile is disabled.`
        : `Used by the roles ${listed}. Their tasks cannot open windows while this profile is disabled.`;
    },
    profileUpdateFailed: (id: string, reason: string, isEnabled: boolean): string =>
      isEnabled
        ? `Could not update ${id}: ${reason}. It is still enabled.`
        : `Could not update ${id}: ${reason}. It is still disabled.`,
    profileShowDetails: 'Details',
    profileHideDetails: 'Hide details',
    profileDetailLoading: 'Reading the profile…',
    profileDetailFailed: (reason: string): string => `Could not read this profile: ${reason}`,
    profileChanged: 'Changed',
    profileShipped: 'Shipped',
    profileShippedValue: (value: string): string => `shipped: ${value}`,
    profileCommandHeading: (os: string): string => `Command on ${os}`,
    profileArgumentLabel: (index: number): string => `Argument ${index}`,
    profileRemoveArgument: (index: number): string => `Remove argument ${index}`,
    profileAddArgument: 'Add argument',
    profileSaveCommand: 'Save',
    profileDiscardCommand: 'Discard',
    profileResetCommand: (shipped: string): string => `Reset to shipped (${shipped})`,
    profileCommandFailed: (reason: string): string => `Could not save the command: ${reason}`,
    profileAliasesHeading: 'Model aliases',
    profileEditInModels: 'Edit in Plans → Models',
    profileLaunchHeading: 'Launch',
    profileEnvironmentHeading: 'Environment',
    profileShowValues: 'Show',
    profileHideValues: 'Hide',
    profileProvidedHeading: 'Provided by yagura',
    profileProvidedIncluded: 'Included',
    profileProvidedLabel: (table: string): string =>
      ({
        mcp: 'MCP server',
        hooks: 'Status hooks',
        mcp_registration: 'MCP registration',
        'model.choices': 'Model list',
        'effort.choices': 'Effort list',
      })[table] ?? table,
    profileSettingsReadOnly: `Turning profiles on or off needs a newer daemon: restart it (yagura-daemon --shutdown). ${RESTART_CLOSES_ALL_WINDOWS}`,

    approvalsTab: 'Approvals',
    approvalsDescription:
      'When an agent asks to open a window or to act on another window, yagura waits for your approval unless that action runs automatically. Actions a window takes on itself, and actions you take in this app, never wait.',
    approvalToolsHeading: 'Agent actions on windows',
    approvalPlansHeading: 'Plans',

    /** One row per gated tool (protocol `DestructiveTool::ALL`), keyed by its wire name. */
    approvalToolInfo: {
      open_window: {
        name: 'Open a window',
        description:
          'Starts a new window that runs a command or a profile, including the windows a handoff or a plan task opens.',
        risk: "An agent can run any command as you, outside its own CLI's permission prompts.",
      },
      send_chat: {
        name: 'Post in a chat room',
        description: "Posts a message to a chat room the agent's window takes part in.",
        risk: 'Agents can talk to each other without you reading each post first.',
      },
      broadcast_chat: {
        name: 'Broadcast to a chat room',
        description: 'Posts a message to every participant of a chat room.',
        risk: 'One agent can reach every window in the room at once.',
      },
      send_message: {
        name: 'Send a message to a window',
        description: 'Types text into another window and submits it.',
        risk: 'An agent can give another agent new instructions.',
      },
      send_handoff: {
        name: 'Hand off work to a window',
        description:
          'Sends a reference and a note to another window, or opens a new window for it.',
        risk: 'An agent can start work in another window or open new ones.',
      },
      send_keys: {
        name: 'Send keys to a window',
        description:
          'Sends control keys such as Enter, Escape or the arrow keys to another window.',
        risk: "An agent can answer another CLI's confirmation prompts.",
      },
      interrupt_window: {
        name: 'Interrupt a window',
        description:
          'Sends an interrupt (Ctrl+C unless the playbook sets another) to another window.',
        risk: "An agent can stop another agent's work in progress.",
      },
      close_window: {
        name: 'Close a window',
        description: 'Ends another window and every process it started.',
        risk: 'An agent can close windows you are still using.',
      },
    },

    approvalToolSwitchLabel: (name: string): string => `Automatically approve: ${name}`,
    planReviewSwitchLabel: 'Review new plans before they run',
    planReviewDescription:
      'A new plan waits for you to approve it before any of its task windows can open.',

    approvalRunsAutomatically: 'Runs automatically',
    approvalWaitsForApproval: 'Waits for approval',
    planReviewFirst: 'Review first',
    planRunsWithoutReview: 'Runs without review',
    planReviewOffRisk: 'Task windows of a new plan open as soon as an agent asks.',

    approvalSettingsNeedNewerDaemon: `Approval settings need a newer daemon: restart it (yagura-daemon --shutdown). ${RESTART_CLOSES_ALL_WINDOWS}`,
    approvalPlaybookFailed:
      'The playbook could not be read when the daemon started, so agents cannot use these ' +
      'actions on other windows, chat is off, and new plans wait for review. Fix ' +
      `playbook.toml, then restart the daemon (yagura-daemon --shutdown). ${RESTART_CLOSES_ALL_WINDOWS}`,
    approvalPlaybookNotConfigured:
      'No playbook is in use, so every action below waits for your approval. Restart the ' +
      `daemon to load the default playbook. ${RESTART_CLOSES_ALL_WINDOWS}`,
    approvalPlaybookUnknown:
      'This daemon reported a playbook state this build does not recognize, so approval ' +
      'settings are read-only until you update yagura.',

    approvalStillRunsAutomatically: APPROVAL_STILL_RUNS_AUTOMATICALLY,
    approvalStillWaitsForApproval: APPROVAL_STILL_WAITS_FOR_APPROVAL,
    planStillReviewsFirst: PLAN_STILL_REVIEWS_FIRST,
    planStillRunsWithoutReview: PLAN_STILL_RUNS_WITHOUT_REVIEW,

    /**
     * Split into a tool line and a plan line, rather than one function taking
     * a discriminant, so this resource never has to spell out the wire words
     * that discriminant would need (this file's own literals must not
     * duplicate a literal already used elsewhere as code, and those two words
     * are already in use as field names at the IPC boundary).
     */
    approvalToolUpdateFailure: (id: string, reason: string, wasOn: boolean): string =>
      `Could not update ${id}: ${reason}. ${
        wasOn ? APPROVAL_STILL_RUNS_AUTOMATICALLY : APPROVAL_STILL_WAITS_FOR_APPROVAL
      }`,
    planUpdateFailure: (id: string, reason: string, wasOn: boolean): string =>
      `Could not update ${id}: ${reason}. ${
        wasOn ? PLAN_STILL_REVIEWS_FIRST : PLAN_STILL_RUNS_WITHOUT_REVIEW
      }`,

    // The Integrations tab: registering yagura's MCP bridge with a CLI the
    // user starts themselves, so that session can open and drive yagura's
    // windows.
    integrationsTab: 'Integrations',
    integrationsDescription:
      "Register yagura's MCP server with a CLI you start yourself, so that session can open " +
      "and drive yagura's windows. A window yagura opens on its own is already connected and " +
      "needs nothing here. yagura runs the CLI's own registration command, shown here before " +
      'it runs.',
    integrationsHostGroup: 'This computer',
    integrationRegistered: 'Registered',
    integrationNotRegistered: 'Not registered',
    integrationDiffers: 'Registered to a different bridge',
    integrationNotInstalled: 'Not installed',
    integrationUnknown: 'Unknown',
    integrationChecking: 'Checking…',
    integrationDiffersIn: (key: string): string => `Registered with a different ${key}`,
    integrationRegisterButton: 'Register',
    integrationUpdateButton: 'Update',
    integrationRemoveButton: 'Remove',
    integrationTomlSetNote: (
      file: string,
      table: readonly string[],
      values: readonly { key: string; value: string }[]
    ): string => {
      const path = table.join('.');
      const changes = values.map((entry) => `${entry.key} = ${entry.value}`).join(', ');
      return `Also sets ${changes} in [${path}] of ${file}.`;
    },
    integrationHookEntriesNote: (file: string, names: readonly string[]): string =>
      names.length === 1
        ? `Also adds the "${names[0] ?? ''}" entry to ${file} so the CLI reports its status.`
        : `Also adds ${names.length} entries to ${file} so the CLI reports its status.`,
    integrationsNoBridge:
      'yagura could not find its own MCP bridge, so no CLI can be registered from here.',
    integrationsNeedNewerDaemon: `Registering a CLI needs a newer daemon: restart it (yagura-daemon --shutdown). ${RESTART_CLOSES_ALL_WINDOWS}`,
    integrationsFooter: (bridge: string): string =>
      `Bridge: ${bridge}. Restart a running CLI session to pick up a change.`,
    integrationsListFailure: 'Could not read the CLI registrations',
    integrationRunFailure: (name: string): string =>
      `Could not run the registration command for ${name}`,

    // The Keyboard tab: the shortcut table, recording a chord, and why a chord
    // or a save is refused.
    keyboardTab: 'Keyboard',
    keyboardDescription:
      'Changes are saved to keymap.toml and apply right away. Keys that reach the programs ' +
      'inside your windows, such as Ctrl+R, are not accepted.',
    keyboardChange: 'Change',
    keyboardReset: 'Reset',
    keyboardRemove: 'Remove',
    keyboardNotAssigned: 'Not assigned',
    keyboardChangedMark: 'Changed',
    keyboardRecording: 'Press the keys…  Esc to cancel',
    keyboardGroupHeading: (group: KebabCase<keyof typeof KEYBOARD_GROUP_HEADINGS>): string =>
      lookup(KEYBOARD_GROUP_HEADINGS, group),
    keyboardActionName: (action: KebabCase<keyof typeof KEYBOARD_ACTION_NAMES>): string =>
      lookup(KEYBOARD_ACTION_NAMES, action),
    keyboardActionNote: (action: KebabCase<keyof typeof KEYBOARD_ACTION_NAMES>): string | null =>
      KEYBOARD_ACTION_NOTES[camelCase(action)] ?? null,
    keyboardRejection: (
      reason: KebabCase<keyof typeof KEYBOARD_REJECTIONS>,
      chordText: string
    ): string => lookup<(chord: string) => string>(KEYBOARD_REJECTIONS, reason)(chordText),
    keyboardTook: (chordText: string, fromName: string): string =>
      `${chordText} was used by “${fromName}”. It now belongs here, and “${fromName}” has no key.`,
    keyboardLost: (chordText: string): string =>
      `Now unassigned: ${chordText} moved to another action.`,
    keyboardSaveFailure: (kind: keyof typeof KEYBOARD_SAVE_FAILURES): string =>
      KEYBOARD_SAVE_FAILURES[kind],
    keyboardEditBlocked: (block: keyof typeof KEYBOARD_EDIT_BLOCKS, location: string): string =>
      KEYBOARD_EDIT_BLOCKS[block](location),
  },

  nav: {
    windows: 'Windows',
    plans: 'Plans',
    chat: 'Chat',
    theme: 'Theme',
    settings: 'Settings',
    windowMenu: 'Window menu',
    windowTabs: 'Window tabs',
  },
};
