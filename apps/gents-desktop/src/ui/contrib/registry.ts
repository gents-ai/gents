/* The one registry every area reads from, keyed by area id. Snapshots are
   cached per area and dropped only on a mutation of that area, so the value
   is referentially stable for useSyncExternalStore and registering a nav row
   never re-renders a session header. A batch (registerMany) touches each
   area once. Copied from hermes-agent's contrib/registry.ts minus the
   nanostores version atom, which has no consumer here yet. */
import type { Contribution } from "./types";

type Listener = () => void;

const EMPTY: readonly Contribution[] = Object.freeze([]);

class ContributionRegistry {
  private byArea = new Map<string, Contribution[]>();
  private snapshot = new Map<string, readonly Contribution[]>();
  private areaListeners = new Map<string, Set<Listener>>();
  private globalListeners = new Set<Listener>();

  /** one contribution; the disposer removes it (inert if replaced since) */
  register = (c: Contribution): (() => void) => this.registerMany([c]);

  /** several at once; the disposer removes them all */
  registerMany = (cs: Contribution[]): (() => void) => {
    cs.forEach((c) => this.put(c));
    this.invalidate(cs.map((c) => c.area));
    return () => this.removeMany(cs);
  };

  /** resolved, sorted, filtered entries; same reference until the area mutates */
  getArea = (area: string): readonly Contribution[] => {
    const cached = this.snapshot.get(area);
    if (cached) return cached;
    const raw = this.byArea.get(area);
    const resolved: readonly Contribution[] =
      !raw || raw.length === 0
        ? EMPTY
        : raw
            .map((c, index) => ({ c, index }))
            .filter(({ c }) => c.enabled !== false && (c.when ? c.when() : true))
            .sort((a, b) => (a.c.order ?? 0) - (b.c.order ?? 0) || a.index - b.index)
            .map(({ c }) => c);
    this.snapshot.set(area, resolved);
    return resolved;
  };

  /** every area that has at least one entry */
  areas = (): string[] => [...this.byArea.keys()];

  /** any mutation; engines that react to every change */
  subscribe = (fn: Listener): (() => void) => {
    this.globalListeners.add(fn);
    return () => {
      this.globalListeners.delete(fn);
    };
  };

  /** one area's mutations; what useContributions subscribes to */
  subscribeArea = (area: string, fn: Listener): (() => void) => {
    const set = this.areaListeners.get(area) ?? new Set<Listener>();
    set.add(fn);
    this.areaListeners.set(area, set);
    return () => {
      set.delete(fn);
      if (set.size === 0) this.areaListeners.delete(area);
    };
  };

  /* a disposer removes the exact entry it registered: a later re-register of
     the same id replaced it, and that newer entry must survive a stale
     disposer (the nav registry's existing contract) */
  private removeMany(entries: Contribution[]) {
    const changed: string[] = [];
    for (const entry of entries) {
      if (this.take(entry)) changed.push(entry.area);
    }
    if (changed.length) this.invalidate(changed);
  }

  private put(c: Contribution) {
    const list = this.byArea.get(c.area) ?? [];
    this.byArea.set(c.area, [...list.filter((e) => e.id !== c.id), c]);
  }

  private take(entry: Contribution): boolean {
    const list = this.byArea.get(entry.area);
    if (!list) return false;
    const next = list.filter((e) => e !== entry);
    if (next.length === list.length) return false;
    if (next.length) this.byArea.set(entry.area, next);
    else this.byArea.delete(entry.area);
    return true;
  }

  private invalidate(areas: readonly string[]) {
    for (const area of new Set(areas)) {
      this.snapshot.delete(area);
      this.areaListeners.get(area)?.forEach((l) => l());
    }
    this.globalListeners.forEach((l) => l());
  }
}

export const registry = new ContributionRegistry();
