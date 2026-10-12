/**
 * Wire-adjacent literal types shared by the daemon-recovery boundary
 * (`ipc/session-boundary.ts`) and the banner model (`banner-model.ts`).
 *
 * Deliberately depends on neither `ipc/` nor `banner-model`: both of those
 * modules import from here, and a dependency the other way would cycle.
 */

/** How far daemon recovery has got (mirrors the Rust `RecoveryStatus` enum). */
export type DaemonRecoveryStatus = 'idle' | 'retrying' | 'exhausted';

/**
 * Why `client.toml` could not be loaded (mirrors the Rust
 * `ClientConfigFailure` enum).
 */
export type ClientConfigFailure = 'not_found' | 'read_failed' | 'invalid_toml' | 'too_large';

/**
 * Why start-up could not resolve which instance this process belongs to
 * (mirrors the Rust `StartupEnvFailure` enum). The rejected identifier is
 * not part of this type: the reason alone reaches the banner.
 */
export type StartupEnvFailure = 'invalid_instance_id' | 'store_override_with_named_instance';
