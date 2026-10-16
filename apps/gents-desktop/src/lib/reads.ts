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
