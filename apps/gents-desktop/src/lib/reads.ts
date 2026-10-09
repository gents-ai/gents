/* Reads whose answers can land out of order, or that should not overlap. */

/**
 * Only the newest of a run of reads is shown: `begin()` starts a read and
 * returns whether that read is still the newest when its answer lands.
 * `supersede()` outdates every read still out, for a write or an answer that
 * knows better than anything they could bring back.
 */
export function newestWins() {
  let newest = 0;
  return {
    begin(): () => boolean {
      const read = ++newest;
      return () => read === newest;
    },
    supersede() {
      newest += 1;
    },
  };
}

/** `newestWins` for each key on its own: a read of one key never outdates
    another's. */
export function newestWinsBy<K>() {
  const newest = new Map<K, number>();
  return {
    begin(key: K): () => boolean {
      const read = (newest.get(key) ?? 0) + 1;
      newest.set(key, read);
      return () => newest.get(key) === read;
    },
    supersede(key: K) {
      newest.set(key, (newest.get(key) ?? 0) + 1);
    },
  };
}

/**
 * One run at a time: a call while `run` is under way joins it instead of
 * starting another, and the next call after it settles starts afresh.
 * `running` says whether a run is under way.
 */
export function singleFlight<T>(run: () => Promise<T>) {
  let pending: Promise<T> | null = null;
  const call = (): Promise<T> =>
    (pending ??= run().finally(() => {
      pending = null;
    }));
  return Object.defineProperty(call, "running", {
    get: () => pending !== null,
  }) as typeof call & { readonly running: boolean };
}
