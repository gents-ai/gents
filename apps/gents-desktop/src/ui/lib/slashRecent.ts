import { browserStorage, storedStrings } from "./storage";

/* The commands and skills picked from the slash menu on this device, newest
   first, so the menu can offer them first. Keys are a row's own key, such as
   `skill:review`; a key the current agent doesn't offer is skipped. */
const KEY = "gents.slashRecent";
const KEPT = 20;
/** how many recent rows lead the menu before anything is typed */
export const RECENT_SHOWN = 3;

export function recentSlash(): string[] {
  try {
    return storedStrings(JSON.parse(browserStorage.getItem(KEY) ?? "[]"));
  } catch {
    return [];
  }
}

export function noteSlash(key: string) {
  const next = [key, ...recentSlash().filter((k) => k !== key)].slice(0, KEPT);
  browserStorage.setItem(KEY, JSON.stringify(next));
}

export type RankedRow = { key: string; name: string; also?: string; group?: string };

/** Rows for a query. With nothing typed, the most recent few lead in a group
    of their own and the rest keep their order. With a query, rows whose name
    starts with it come before rows that only contain it, and among equals
    the more recently used comes first. */
export function rankSlash<Row extends RankedRow>(
  rows: readonly Row[],
  query: string,
  recent: readonly string[],
): Row[] {
  const used = (row: Row) => {
    const at = recent.indexOf(row.key);
    return at === -1 ? Infinity : at;
  };
  if (query === "") {
    const lead = rows
      .filter((row) => used(row) < RECENT_SHOWN)
      .sort((a, b) => used(a) - used(b))
      .map((row) => ({ ...row, group: "Recent" }));
    const shown = new Set(lead.map((row) => row.key));
    return [...lead, ...rows.filter((row) => !shown.has(row.key))];
  }
  const q = query.toLowerCase();
  const fit = (row: Row) => {
    if (row.name.toLowerCase().startsWith(q)) return 0;
    return `${row.name} ${row.also ?? ""}`.toLowerCase().includes(q) ? 1 : 2;
  };
  return rows
    .map((row, index) => ({ row, index, fit: fit(row), used: used(row) }))
    .filter((entry) => entry.fit < 2)
    .sort((a, b) => a.fit - b.fit || a.used - b.used || a.index - b.index)
    .map(({ row }) => ({ ...row, group: undefined }));
}
