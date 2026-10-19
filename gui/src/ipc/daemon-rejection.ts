/**
 * Reading the code out of a request the daemon refused. The Tauri core
 * renders such a refusal as `the daemon rejected the request: <code>` with
 * `: <reason>` after it when the daemon gave one.
 */

const REJECTED_PREFIX = 'the daemon rejected the request: ';

/**
 * The code a daemon refusal carries, `null` when `error` is not that shape.
 *
 * @param error the value a command rejected with
 */
export function daemonRejectionCode(error: unknown): string | null {
  const text = typeof error === 'string' ? error : error instanceof Error ? error.message : null;
  if (text === null || !text.startsWith(REJECTED_PREFIX)) {
    return null;
  }
  const rest = text.slice(REJECTED_PREFIX.length);
  const separator = rest.indexOf(': ');
  return separator === -1 ? rest : rest.slice(0, separator);
}
