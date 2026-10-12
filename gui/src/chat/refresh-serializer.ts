/**
 * Wraps `run` so concurrent callers share runs instead of racing duplicate
 * reads: at most one run is in flight, and calls arriving mid-run are folded
 * into a single follow-up run whose completion they all wait for, so none of
 * them returns before a run that started after its own call has settled.
 *
 * The chain stops at that one follow-up (a steady stream of calls cannot
 * stretch an early caller's wait without bound), and a rejected `run` ends
 * its turn like a successful one, so a failed refresh leaves nothing stuck.
 */
export function serializeRefresh(run: () => Promise<void>): () => Promise<void> {
  let inFlight: Promise<void> | null = null;
  let queued: Promise<void> | null = null;

  function refresh(): Promise<void> {
    const current = inFlight;
    if (current === null) {
      const started = run().finally(() => {
        inFlight = null;
      });
      inFlight = started;
      return started;
    }
    const next = (): Promise<void> => {
      queued = null;
      return refresh();
    };
    // `inFlight`'s own continuation clears it before this one runs, so the
    // follow-up starts a fresh run rather than queueing behind itself.
    queued ??= current.then(next, next);
    return queued;
  }

  return refresh;
}
