/**
 * The rows of the command picker: which windows and actions are listed, in
 * what order and under which heading, what part of each visible text is
 * marked, and where the selection goes. The caller hands in the texts as the
 * user sees them and whether each action can run, so nothing here reads the
 * locale, the IPC or the stores.
 */

import { createGraphemeSegmenter } from '../display/graphemes';
import type { ActionBlockReason, ActionVerdict } from '../keymap/action-availability';
import { ACTION_IDS, type ActionId } from '../keymap/key-binding';
import { STATUS_PRESENTATION } from '../status-presentation';
import { isProminentWindow, needsYou, type NeedsYouInput } from '../status-tone';

/** Windows are looked up by name; commands are the actions, typed after a leading `>`. */
export type PickerMode = 'windows' | 'commands';

/** One window as its row shows it. Every text is the one drawn on screen. */
export interface PickerWindow extends NeedsYouInput {
  windowId: string;
  name: string;
  /** The room's name, or the folder for a window in no room. */
  place: string;
  cwd: string;
  vendor: string | null;
  statusWords: string;
}

/** One action as its row shows it. */
export interface PickerAction {
  id: ActionId;
  name: string;
  /** Extra words, separated by spaces, that find the action but are not shown. */
  keywords: string;
  section: string;
  verdict: ActionVerdict;
}

/** What the rows are built from. */
export interface PickerSource<W extends PickerWindow> {
  /** In sidebar order. */
  windows: readonly W[];
  currentId: string | null;
  /** In any order: rows follow the action order. */
  actions: readonly PickerAction[];
}

/** A part of a shown text, in code points; `end` is excluded. */
export interface MarkSpan {
  start: number;
  end: number;
}

export interface PickerWindowRow<W extends PickerWindow> {
  kind: 'window';
  key: string;
  window: W;
  isCurrent: boolean;
  marks: { name: MarkSpan[]; place: MarkSpan[]; cwd: MarkSpan[]; vendor: MarkSpan[] };
}

export interface PickerActionRow {
  kind: 'action';
  key: string;
  action: PickerAction;
  blocked: ActionBlockReason | null;
  /** The section name is shown beside the row, once the rows are no longer grouped. */
  showsSection: boolean;
  marks: { name: MarkSpan[] };
}

export type PickerRow<W extends PickerWindow> = PickerWindowRow<W> | PickerActionRow;

export type PickerHeading =
  { kind: 'needs-you' } | { kind: 'other-windows' } | { kind: 'actions'; name: string } | null;

export interface PickerSection<W extends PickerWindow> {
  heading: PickerHeading;
  rows: PickerRow<W>[];
}

/** Why nothing, or only the New window offer, is listed. `query` is what was typed. */
export type PickerEmpty =
  | { kind: 'no-windows' }
  | { kind: 'no-window-match'; query: string }
  | { kind: 'no-command-match'; query: string }
  | null;

export interface PickerState<W extends PickerWindow> {
  input: string;
  mode: PickerMode;
  sections: PickerSection<W>[];
  empty: PickerEmpty;
  /** The key of the selected row. */
  selected: string | null;
  /** The windows there were when the rows were last built or refreshed; any other is new. */
  knownWindowIds: ReadonlySet<string>;
}

const COMMAND_PREFIX = '>';

/** The input a mode opens with, and where the caret goes in it. */
export function openingInput(mode: PickerMode): { input: string; caret: number } {
  return mode === 'commands' ? { input: COMMAND_PREFIX, caret: 1 } : { input: '', caret: 0 };
}

/** The picker as it opens in `mode`, with the first row selected. */
export function openPicker<W extends PickerWindow>(
  mode: PickerMode,
  source: PickerSource<W>
): PickerState<W> {
  return build(openingInput(mode).input, source);
}

/** Builds the rows again for `input`; the mode follows its leading `>`. */
export function typeInput<W extends PickerWindow>(
  _state: PickerState<W>,
  input: string,
  source: PickerSource<W>
): PickerState<W> {
  return build(input, source);
}

/**
 * The open key of `mode` pressed while the picker is open: the other mode's key
 * opens the picker as that mode opens, and the key of the mode shown does nothing.
 * @returns the new state, or `null` when nothing changes
 */
export function pressOpenKey<W extends PickerWindow>(
  state: PickerState<W>,
  mode: PickerMode,
  source: PickerSource<W>
): PickerState<W> | null {
  return state.mode === mode ? null : openPicker(mode, source);
}

/**
 * Brings the rows up to date without moving them: each row shows its window or
 * action as it is now, a window that is gone loses its row, and a new window is
 * added at the end of where it would go now (only when it matches what is
 * typed). The selection stays, unless its row is gone.
 */
export function refreshPicker<W extends PickerWindow>(
  state: PickerState<W>,
  source: PickerSource<W>
): PickerState<W> {
  const sections =
    state.mode === 'commands'
      ? refreshActionSections(state, source)
      : refreshWindowSections(state, source);
  return finish(state.input, state.mode, sections, source, (rows) =>
    rows.some((row) => row.key === state.selected) ? state.selected : null
  );
}

/**
 * Moves the selection by `distance` rows, stopping at either end. With nothing
 * selected, down goes to the first row and up to the last.
 */
export function moveSelection<W extends PickerWindow>(
  state: PickerState<W>,
  direction: 'down' | 'up',
  distance: number
): PickerState<W> {
  const rows = rowsOf(state);
  if (rows.length === 0) {
    return state;
  }
  const at = rows.findIndex((row) => row.key === state.selected);
  let next: number;
  if (at === -1) {
    next = direction === 'down' ? 0 : rows.length - 1;
  } else {
    const moved = direction === 'down' ? at + distance : at - distance;
    next = Math.min(rows.length - 1, Math.max(0, moved));
  }
  return { ...state, selected: rows[next]?.key ?? null };
}

/** Selects the row under the pointer. */
export function pointAt<W extends PickerWindow>(
  state: PickerState<W>,
  key: string
): PickerState<W> {
  return rowsOf(state).some((row) => row.key === key) ? { ...state, selected: key } : state;
}

/** The row Enter runs: the selected row when it can run, else `null`. */
export function chosenRow<W extends PickerWindow>(state: PickerState<W>): PickerRow<W> | null {
  const row = rowsOf(state).find((candidate) => candidate.key === state.selected);
  if (row === undefined || (row.kind === 'action' && row.blocked !== null)) {
    return null;
  }
  return row;
}

function rowsOf<W extends PickerWindow>(state: PickerState<W>): PickerRow<W>[] {
  return state.sections.flatMap((section) => section.rows);
}

// --- Building the rows ---

function build<W extends PickerWindow>(input: string, source: PickerSource<W>): PickerState<W> {
  const mode: PickerMode = input.startsWith(COMMAND_PREFIX) ? 'commands' : 'windows';
  const query = prepareQuery(input);
  const sections =
    mode === 'commands' ? actionSections(query, source) : windowSections(query, source);
  return finish(input, mode, sections, source, (rows) => rows[0]?.key ?? null);
}

function finish<W extends PickerWindow>(
  input: string,
  mode: PickerMode,
  sections: PickerSection<W>[],
  source: PickerSource<W>,
  selectedIn: (rows: PickerRow<W>[]) => string | null
): PickerState<W> {
  const kept = sections.filter((section) => section.rows.length > 0);
  const rows = kept.flatMap((section) => section.rows);
  return {
    input,
    mode,
    sections: kept,
    empty: emptyOf(mode, prepareQuery(input), source, rows.length),
    selected: selectedIn(rows),
    knownWindowIds: new Set(source.windows.map((w) => w.windowId)),
  };
}

function emptyOf<W extends PickerWindow>(
  mode: PickerMode,
  query: Query,
  source: PickerSource<W>,
  rowCount: number
): PickerEmpty {
  if (mode === 'windows' && query.words.length === 0 && source.windows.length === 0) {
    return { kind: 'no-windows' };
  }
  if (rowCount > 0) {
    return null;
  }
  return mode === 'windows'
    ? { kind: 'no-window-match', query: query.text }
    : { kind: 'no-command-match', query: query.text };
}

function windowSections<W extends PickerWindow>(
  query: Query,
  source: PickerSource<W>
): PickerSection<W>[] {
  if (query.words.length === 0) {
    if (source.windows.length === 0) {
      return [{ heading: null, rows: newWindowOffer(source) }];
    }
    const needing = source.windows.filter(needsYou);
    const others = source.windows.filter((w) => !needsYou(w));
    return [
      {
        heading: NEEDS_YOU,
        rows: sortWindows(needing, source).map((w) => windowRow(w, null, source)),
      },
      {
        heading: OTHER_WINDOWS,
        rows: sortWindows(others, source).map((w) => windowRow(w, null, source)),
      },
    ];
  }
  const matched = source.windows.flatMap((w) => {
    const match = matchWindow(w, query);
    return match === null ? [] : [{ w, match }];
  });
  const order = sidebarOrder(source);
  matched.sort((a, b) => a.match.stage - b.match.stage || compareByAttention(a.w, b.w, order));
  const rows = moveCurrentLast(matched, ({ w }) => w.windowId === source.currentId).map(
    ({ w, match }) => windowRow(w, match.marks, source)
  );
  return [{ heading: null, rows }];
}

const NEEDS_YOU = { kind: 'needs-you' } as const;
const OTHER_WINDOWS = { kind: 'other-windows' } as const;

/** With nothing open, the windows mode offers to open one, so it is never a dead end. */
function newWindowOffer<W extends PickerWindow>(source: PickerSource<W>): PickerRow<W>[] {
  const action = source.actions.find((candidate) => candidate.id === 'open-window-form');
  return action === undefined ? [] : [actionRow(action, null, false)];
}

/** Sorts one section with nothing typed; the current window goes last. */
function sortWindows<W extends PickerWindow>(windows: readonly W[], source: PickerSource<W>): W[] {
  const order = sidebarOrder(source);
  const sorted = [...windows].sort((a, b) => compareByAttention(a, b, order));
  return moveCurrentLast(sorted, (w) => w.windowId === source.currentId);
}

/**
 * Prominent windows first, among them those needing the user, then the status
 * rank and the sidebar order. Ranking unknown above a reported input wait
 * keeps an unconfirmed status from reading as a quiet one.
 */
function compareByAttention(
  a: PickerWindow,
  b: PickerWindow,
  order: ReadonlyMap<string, number>
): number {
  return (
    Number(isProminentWindow(b)) - Number(isProminentWindow(a)) ||
    Number(needsYou(b)) - Number(needsYou(a)) ||
    STATUS_PRESENTATION[a.detected.status].rank - STATUS_PRESENTATION[b.detected.status].rank ||
    (order.get(a.windowId) ?? 0) - (order.get(b.windowId) ?? 0)
  );
}

function sidebarOrder<W extends PickerWindow>(source: PickerSource<W>): Map<string, number> {
  return new Map(source.windows.map((w, index) => [w.windowId, index]));
}

function moveCurrentLast<T>(items: T[], isCurrent: (item: T) => boolean): T[] {
  return [...items.filter((item) => !isCurrent(item)), ...items.filter(isCurrent)];
}

function windowRow<W extends PickerWindow>(
  window: W,
  marks: PickerWindowRow<W>['marks'] | null,
  source: PickerSource<W>
): PickerWindowRow<W> {
  return {
    kind: 'window',
    key: `window:${window.windowId}`,
    window,
    isCurrent: window.windowId === source.currentId,
    marks: marks ?? { name: [], place: [], cwd: [], vendor: [] },
  };
}

function actionSections<W extends PickerWindow>(
  query: Query,
  source: PickerSource<W>
): PickerSection<W>[] {
  const listed = [...source.actions]
    .filter((action) => action.id !== 'open-command-palette')
    .sort((a, b) => ACTION_IDS.indexOf(a.id) - ACTION_IDS.indexOf(b.id));
  if (query.words.length === 0) {
    const bySection = new Map<string, PickerRow<W>[]>();
    for (const action of listed) {
      const rows = bySection.get(action.section) ?? [];
      rows.push(actionRow(action, null, false));
      bySection.set(action.section, rows);
    }
    return Array.from(bySection, ([name, rows]): PickerSection<W> => ({
      heading: { kind: 'actions', name },
      rows,
    }));
  }
  const matched = listed.flatMap((action) => {
    const match = matchAction(action, query);
    return match === null ? [] : [{ action, match }];
  });
  // Array sort is stable, so the action order decides among the rest.
  matched.sort(
    (a, b) =>
      a.match.stage - b.match.stage || Number(!a.action.verdict.ok) - Number(!b.action.verdict.ok)
  );
  return [
    {
      heading: null,
      rows: matched.map(({ action, match }) => actionRow(action, match.marks, true)),
    },
  ];
}

function actionRow(
  action: PickerAction,
  marks: { name: MarkSpan[] } | null,
  showsSection: boolean
): PickerActionRow {
  return {
    kind: 'action',
    key: `action:${action.id}`,
    action,
    blocked: action.verdict.ok ? null : action.verdict.reason,
    showsSection,
    marks: marks ?? { name: [] },
  };
}

// --- Keeping the rows while the picker is open ---

function refreshWindowSections<W extends PickerWindow>(
  state: PickerState<W>,
  source: PickerSource<W>
): PickerSection<W>[] {
  const query = prepareQuery(state.input);
  const byId = new Map(source.windows.map((w) => [w.windowId, w]));
  const marksFor = (w: W) =>
    query.words.length === 0 ? null : (matchWindow(w, query)?.marks ?? null);
  const sections: PickerSection<W>[] = state.sections.map((section) => ({
    heading: section.heading,
    rows: section.rows.flatMap((row): PickerRow<W>[] => {
      if (row.kind === 'action') {
        // The only action row here is the New window offer, which goes once a window exists.
        return source.windows.length === 0 ? refreshActionRow(row, source) : [];
      }
      const now = byId.get(row.window.windowId);
      return now === undefined ? [] : [windowRow(now, marksFor(now), source)];
    }),
  }));

  for (const w of source.windows) {
    if (state.knownWindowIds.has(w.windowId)) {
      continue;
    }
    if (query.words.length > 0) {
      const match = matchWindow(w, query);
      if (match !== null) {
        sectionFor(sections, null, sections.length).rows.push(windowRow(w, match.marks, source));
      }
      continue;
    }
    const needing = needsYou(w);
    // The section order is fixed: Needs you above the other windows.
    const at = needing
      ? sections.findIndex((section) => section.heading?.kind === 'other-windows')
      : sections.length;
    sectionFor(
      sections,
      needing ? NEEDS_YOU : OTHER_WINDOWS,
      at === -1 ? sections.length : at
    ).rows.push(windowRow(w, null, source));
  }

  const hasRows = sections.some((section) => section.rows.length > 0);
  if (!hasRows && query.words.length === 0 && source.windows.length === 0) {
    return [{ heading: null, rows: newWindowOffer(source) }];
  }
  return sections;
}

/** The section with `heading`, inserted at `at` when there is none. */
function sectionFor<W extends PickerWindow>(
  sections: PickerSection<W>[],
  heading: PickerHeading,
  at: number
): PickerSection<W> {
  const found = sections.find((section) => section.heading?.kind === heading?.kind);
  if (found !== undefined) {
    return found;
  }
  const created: PickerSection<W> = { heading, rows: [] };
  sections.splice(at, 0, created);
  return created;
}

function refreshActionSections<W extends PickerWindow>(
  state: PickerState<W>,
  source: PickerSource<W>
): PickerSection<W>[] {
  return state.sections.map((section) => ({
    heading: section.heading,
    rows: section.rows.flatMap((row) =>
      row.kind === 'action' ? refreshActionRow(row, source) : []
    ),
  }));
}

function refreshActionRow<W extends PickerWindow>(
  row: PickerActionRow,
  source: PickerSource<W>
): PickerRow<W>[] {
  const now = source.actions.find((action) => action.id === row.action.id);
  return now === undefined ? [] : [actionRow(now, row.marks, row.showsSection)];
}

// --- Matching ---

interface Query {
  /** What was typed after the `>`, trimmed: the text an empty result quotes back. */
  text: string;
  /** The folded words, each once. Empty when nothing is typed. */
  words: string[];
  /** The folded input with its runs of spaces made one and its ends trimmed. */
  whole: string;
}

function prepareQuery(input: string): Query {
  const bare = input.startsWith(COMMAND_PREFIX) ? input.slice(COMMAND_PREFIX.length) : input;
  const whole = collapseSpaces(fold(bare).folded);
  return {
    text: bare.trim(),
    words: whole === '' ? [] : Array.from(new Set(whole.split(' '))),
    whole,
  };
}

function collapseSpaces(text: string): string {
  return text.replace(/\s+/gu, ' ').trim();
}

/** A row's match: its stage (lower comes first) and the marked parts of its texts. */
interface Match<M> {
  stage: number;
  marks: M;
}

/** Stage 0 is the whole name; 1 its start; 2 a word start in it; 3 its middle; 4 another field. */
const OTHER_FIELD_STAGE = 4;

function matchWindow(
  w: PickerWindow,
  query: Query
): Match<PickerWindowRow<PickerWindow>['marks']> | null {
  const name = fold(w.name);
  const place = fold(w.place);
  const cwd = fold(w.cwd);
  const vendor = w.vendor === null ? null : fold(w.vendor);
  const others = [place.folded, cwd.folded, vendor?.folded ?? '', fold(w.statusWords).folded];
  const stage = stageOf(name.folded, others, query);
  if (stage === null) {
    return null;
  }
  return {
    stage,
    marks: {
      name: stage === 0 ? [wholeSpan(name)] : marksOf(name, query.words),
      place: marksOf(place, query.words),
      cwd: marksOf(cwd, query.words),
      vendor: vendor === null ? [] : marksOf(vendor, query.words),
    },
  };
}

function matchAction(action: PickerAction, query: Query): Match<{ name: MarkSpan[] }> | null {
  const name = fold(action.name);
  const others = [fold(action.keywords).folded, fold(action.section).folded];
  const stage = stageOf(name.folded, others, query);
  if (stage === null) {
    return null;
  }
  return { stage, marks: { name: stage === 0 ? [wholeSpan(name)] : marksOf(name, query.words) } };
}

/**
 * The stage of a row, or `null` when some word is in none of its fields. A
 * whole-name match is stage 0 by itself; otherwise the row takes the stage of
 * its weakest word.
 */
function stageOf(name: string, others: readonly string[], query: Query): number | null {
  let worst = 0;
  for (const word of query.words) {
    const stage =
      nameStage(name, word) ??
      (others.some((field) => field.includes(word)) ? OTHER_FIELD_STAGE : null);
    if (stage === null) {
      return null;
    }
    worst = Math.max(worst, stage);
  }
  return collapseSpaces(name) === query.whole ? 0 : worst;
}

function nameStage(name: string, word: string): number | null {
  let best: number | null = null;
  for (let at = name.indexOf(word); at !== -1; at = name.indexOf(word, at + 1)) {
    if (at === 0) {
      return 1;
    }
    best = Math.min(best ?? 3, startsWord(name, at) ? 2 : 3);
  }
  return best;
}

// A mark joins the letter before it, so a word starts after anything but these.
const WORD_CHAR = /[\p{L}\p{N}\p{M}]/u;

function startsWord(text: string, at: number): boolean {
  const before = Array.from(text.slice(Math.max(0, at - 2), at)).pop();
  return before === undefined || !WORD_CHAR.test(before);
}

/** Where each word is first found in a text, joined where they overlap or touch. */
function marksOf(text: FoldedText, words: readonly string[]): MarkSpan[] {
  const spans = words.flatMap((word) => {
    const at = text.folded.indexOf(word);
    return at === -1 ? [] : [spanOf(text, at, at + word.length)];
  });
  spans.sort((a, b) => a.start - b.start);
  const joined: MarkSpan[] = [];
  for (const span of spans) {
    const last = joined[joined.length - 1];
    if (last !== undefined && span.start <= last.end) {
      last.end = Math.max(last.end, span.end);
    } else {
      joined.push({ ...span });
    }
  }
  return joined;
}

function wholeSpan(text: FoldedText): MarkSpan {
  return { start: 0, end: text.clusterStarts[text.clusterStarts.length - 1] ?? 0 };
}

// --- Folding ---

/**
 * A text folded cluster by cluster, with the way back: `clusterOfUnit[i]` is the
 * cluster the folded UTF-16 unit `i` came from, and `clusterStarts[c]` the code
 * point where cluster `c` starts in the shown text (one more entry for the end).
 */
interface FoldedText {
  folded: string;
  clusterOfUnit: number[];
  clusterStarts: number[];
}

// Older WebKit lacks Intl.Segmenter. There, a base character with its combining
// marks and half-width voiced marks is one cluster, so a decomposed or half-width
// ga still folds to the ga typed through an IME.
const segmenter = createGraphemeSegmenter();
const FALLBACK_CLUSTER = /\P{M}[\p{M}\u{FF9E}\u{FF9F}]*|\p{M}+/gu;

function clustersOf(text: string): string[] {
  if (segmenter === null) {
    return text.match(FALLBACK_CLUSTER) ?? [];
  }
  return Array.from(segmenter.segment(text), (part) => part.segment);
}

function fold(text: string): FoldedText {
  let folded = '';
  const clusterOfUnit: number[] = [];
  const clusterStarts: number[] = [0];
  let codePoints = 0;
  clustersOf(text).forEach((cluster, index) => {
    const part = cluster.normalize('NFKC').toLowerCase();
    folded += part;
    for (let unit = 0; unit < part.length; unit += 1) {
      clusterOfUnit.push(index);
    }
    codePoints += Array.from(cluster).length;
    clusterStarts.push(codePoints);
  });
  return { folded, clusterOfUnit, clusterStarts };
}

/** The shown span of the folded units `[start, end)`, widened to whole clusters. */
function spanOf(text: FoldedText, start: number, end: number): MarkSpan {
  const first = text.clusterOfUnit[start] ?? 0;
  const last = text.clusterOfUnit[end - 1] ?? first;
  return { start: text.clusterStarts[first] ?? 0, end: text.clusterStarts[last + 1] ?? 0 };
}
