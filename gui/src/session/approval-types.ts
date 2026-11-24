/**
 * Wire-adjacent literal types shared by the approval boundary
 * (`ipc/session-boundary.ts`) and the notice model (`notices/notice-model.ts`).
 *
 * Deliberately loads neither `ipc/` nor `ui-messages` at runtime: both of
 * those modules import from here, and a dependency the other way would cycle.
 * The one import from `ui-messages` is type-only, as is its import from here,
 * so it is erased and neither loads the other.
 */
import type { UiText } from '../display/ui-text';
import type { DisplayLine } from '../display/untrusted-text';
import type { TitleParts } from '../ui-messages';

/**
 * Why a pending approval request ended without a decision from the approver
 * being told about it (mirrors the Rust `WithdrawReason` enum).
 *
 * `'unknown'` is this side's normalization of a wire value this build does
 * not recognize (the daemon's serde `#[serde(other)]`); it is never sent by
 * this build's own daemon.
 */
export type WithdrawReason = 'resolved' | 'timeout' | 'requester_disconnected' | 'unknown';

/**
 * What kind of party a pending approval request came from (mirrors the Rust
 * `ApprovalRequesterKind` enum). `'unknown'` is this side's normalization of
 * an unrecognized wire value, the same fail-safe treatment as
 * {@link WithdrawReason}.
 */
export type ApprovalRequesterKind = 'external' | 'window' | 'unknown';

/**
 * The mark the daemon draws from a window's ID (mirrors the Rust `WindowMark`).
 * Bit `row * 3 + column` of `cells` fills that cell of the left three columns of
 * five rows, which the right two columns mirror; `hue` is one of 12 steps.
 */
export interface WindowMark {
  cells: number;
  hue: number;
}

/**
 * What an approval heading and a card name a party by: a window's name, words
 * that stand in for a party, or a room's name with the words around it. Readers
 * branch on `kind` exhaustively, so a new kind cannot be dropped unnoticed.
 */
export type HeadlineParty =
  | { kind: 'name'; name: DisplayLine }
  | { kind: 'words'; words: UiText }
  | { kind: 'room'; parts: TitleParts };

/**
 * An approval heading in the order it reads, so a view can set each name apart
 * from the words, and the sentence those make.
 */
export interface HeadlineParts {
  from: HeadlineParty;
  verb: UiText;
  /** A tool this build does not know, named rather than guessed at. */
  tool: DisplayLine | null;
  /** The words between the tool and what it would act on. */
  toolLink: UiText;
  /** `null` for a tool this build does not know that names nothing to act on. */
  to: HeadlineParty | null;
  sentence: UiText;
}

/** What every side of a request is drawn with, whatever it is. */
interface ApprovalPartyCommon {
  /** The same value the heading names this party by. */
  label: HeadlineParty;
  /** The launch profile a window was opened from; `null` for one opened from a command. */
  profile: DisplayLine | null;
  /** A window's status when the request was submitted, or that it has since closed. */
  status: UiText | null;
}

/**
 * One side of a request, as one card of the dialog draws it: a window, a client
 * that is no window, or what a request names in place of a window. Only a window
 * has an ID and a mark, and it has both.
 */
export type ApprovalParty =
  | (ApprovalPartyCommon & { kind: 'window'; window: { id: DisplayLine; mark: WindowMark } })
  | (ApprovalPartyCommon & { kind: 'external' | 'untargeted' });

/** Everything the approval dialog, and the notice of an expired request, draw a request from. */
export interface ApprovalFacts {
  headline: HeadlineParts;
  /** The gated tool's name, prepared for display. */
  tool: DisplayLine;
  from: ApprovalParty;
  to: ApprovalParty;
  /** Heads what the request would send or run; `null` for a tool that carries nothing. */
  bodyHeading: UiText | null;
}
