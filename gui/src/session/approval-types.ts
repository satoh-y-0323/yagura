/**
 * Wire-adjacent literal types shared by the approval boundary
 * (`ipc/session-boundary.ts`) and the banner model (`banner-model.ts`).
 *
 * Deliberately depends on neither `ipc/` nor `ui-messages`: both of those
 * modules import from here, and a dependency the other way would cycle.
 */

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
