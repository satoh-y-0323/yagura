/**
 * Turns a boundary failure into the one line the user reads.
 *
 * A notice carries only the operation that could not be carried out and the
 * detail whichever layer actually knows the cause produced. Nothing here
 * classifies the failure: the summary is chosen by call site and is true for
 * every failure that site can hit, and the cause is whatever the detail says.
 */
import { describeOperationFailure, errors } from './ui-messages';

/** The boundary failures the WebView reports, one member per call site. */
export type BoundaryOperation =
  | 'sessionEvent'
  | 'sessionEvents'
  | 'windowList'
  | 'chatChanged'
  | 'themes'
  | 'detectionsRefresh'
  | 'detectionsList'
  | 'profilesIncomplete'
  | 'profilesList';

/** One boundary failure: what could not be done, and what was reported about it. */
export interface BoundaryNotice {
  /** The operation that failed. */
  operation: BoundaryOperation;
  /** The value the failure carried: a daemon diagnosis, a rejection, a thrown error. */
  detail: unknown;
}

/**
 * The fixed summary per operation. Written as a `Record` over the union so a
 * tenth call site cannot be added without deciding what it says.
 */
const SUMMARIES: Record<BoundaryOperation, string> = {
  sessionEvent: errors.boundarySessionEvent,
  sessionEvents: errors.boundarySessionEvents,
  windowList: errors.boundaryWindowList,
  chatChanged: errors.boundaryChatChanged,
  themes: errors.boundaryThemes,
  detectionsRefresh: errors.boundaryDetectionsRefresh,
  detectionsList: errors.boundaryDetectionsList,
  profilesIncomplete: errors.boundaryProfilesIncomplete,
  profilesList: errors.boundaryProfilesList,
};

/** True if the value is a list of strings, the shape a daemon warning list takes. */
function isStringList(value: unknown): value is readonly string[] {
  return Array.isArray(value) && value.every((entry: unknown) => typeof entry === 'string');
}

/**
 * Reduces a detail that is a list of diagnoses to the single line the display
 * carries. A general rule of the detail's type, not a per-operation case.
 */
function collapseDetail(detail: unknown): unknown {
  if (!isStringList(detail)) {
    return detail;
  }
  const first = detail[0];
  if (first === undefined) {
    return '';
  }
  return detail.length === 1 ? first : errors.boundaryDetailWithCount(detail.length, first);
}

/**
 * Builds the display text for one boundary failure: the operation's summary,
 * followed by the reported detail when there is one.
 *
 * @param notice the operation that failed and what was reported about it
 */
export function describeBoundaryNotice(notice: BoundaryNotice): string {
  return describeOperationFailure(SUMMARIES[notice.operation], collapseDetail(notice.detail));
}
