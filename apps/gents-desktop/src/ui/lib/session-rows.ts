import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { sessionKeyOf } from "../../hooks/fleetStore";
import { isLive } from "./live";

export type SessionRow = {
  session: SessionSummary;
  /** the sessions it started that the list holds */
  started: number;
  /** of those, the ones still working, or with work running under them */
  running: number;
  /** while narrowed: the sessions between the level shown and this one */
  path: readonly SessionSummary[] | null;
  /** how far it is opened out under a row of the level */
  depth: number;
  /** its started sessions show under it, on this level */
  open: boolean;
  /** the starter the list holds, and the level that starter sits on (null
      for the top); absent for a top session */
  starter: { key: string; level: string | null } | null;
  /** of the sessions it started, the ones its open group leaves out */
  hidden: number;
};

/* an open row shows this many of what it started; the rest are a level of
   their own away */
export const OPENED_SHOWN = 5;

/**
 * One level of `sessions` (the sessions in scope, in list order): the ones
 * `under` started, or the top ones when `under` is null.
 *
 * - A session sits under the nearest starter the list holds (`parentOf`),
 *   through starters it leaves out; one with none listed is a top session.
 * - `trail` runs from the top to `under`. A starter the list no longer holds
 *   shows the top level, so a narrowed pick can't strand the person.
 * - A row the person opened on this level (`isOpen`) shows what it started
 *   under it, one level deeper, and so on for the rows opened there: the
 *   first `OPENED_SHOWN`, and any still working beyond them.
 * - While narrowed (`matches`), the level is flat: every match at or below
 *   it, each with the path from the level to it.
 *
 * `shown` counts the matches (every session on the level when not narrowed),
 * out of `total`, the sessions at or below the level.
 */
export function sessionRows({
  sessions,
  parentOf,
  under,
  matches,
  isOpen = () => false,
}: {
  sessions: readonly SessionSummary[];
  parentOf: (session: SessionSummary) => SessionSummary | null;
  under: string | null;
  /** null when nothing narrows the list */
  matches: ((session: SessionSummary) => boolean) | null;
  isOpen?: (key: string) => boolean;
}): { rows: SessionRow[]; trail: SessionSummary[]; shown: number; total: number } {
  const listed = new Map(sessions.map((s) => [sessionKeyOf(s), s]));
  const starterOf = new Map<string, string>();
  for (const session of sessions) {
    const parent = parentOf(session);
    const key = parent && sessionKeyOf(parent);
    if (key && listed.has(key)) starterOf.set(sessionKeyOf(session), key);
  }
  /* a loop in who started whom has no top: each in it stands on its own */
  const lineOf = (key: string) => {
    const line = [key];
    for (let at = starterOf.get(key); at; at = starterOf.get(at)) {
      if (line.includes(at)) return [key];
      line.unshift(at);
    }
    return line;
  };
  const tops: SessionSummary[] = [];
  const startedBy = new Map<string, SessionSummary[]>();
  for (const session of sessions) {
    const line = lineOf(sessionKeyOf(session));
    if (line.length === 1) {
      tops.push(session);
      continue;
    }
    const starter = line[line.length - 2]!;
    const kin = startedBy.get(starter);
    if (kin) kin.push(session);
    else startedBy.set(starter, [session]);
  }
  const childrenOf = (key: string | null) =>
    key === null ? tops : (startedBy.get(key) ?? []);

  const level = under && listed.has(under) ? under : null;
  const trail = level ? lineOf(level).map((key) => listed.get(key)!) : [];

  const busy = new Map<string, boolean>();
  const working = (session: SessionSummary): boolean => {
    const key = sessionKeyOf(session);
    let result = busy.get(key);
    if (result === undefined) {
      result = isLive(session.turnState) || childrenOf(key).some((s) => working(s));
      busy.set(key, result);
    }
    return result;
  };
  const row = (
    session: SessionSummary,
    path: readonly SessionSummary[] | null,
    depth: number,
  ): SessionRow => {
    const key = sessionKeyOf(session);
    const kin = childrenOf(key);
    const starter = starterOf.get(key);
    return {
      session,
      started: kin.length,
      running: kin.filter(working).length,
      path,
      depth,
      open: !path && kin.length > 0 && isOpen(key),
      hidden: 0,
      starter:
        starter && lineOf(key).length > 1
          ? {
              key: starter,
              level: lineOf(starter).length > 1 ? starterOf.get(starter)! : null,
            }
          : null,
    };
  };

  const rows: SessionRow[] = [];
  let total = 0;
  const walk = (key: string | null, path: SessionSummary[]) => {
    for (const session of childrenOf(key)) {
      total += 1;
      if (matches?.(session)) rows.push(row(session, path, 0));
      walk(sessionKeyOf(session), [...path, session]);
    }
  };
  walk(level, []);
  if (matches) return { rows, trail, shown: rows.length, total };
  const openOut = (key: string | null, depth: number, from: SessionRow | null) => {
    const kin = childrenOf(key);
    const drawn = from ? kin.filter((s, n) => n < OPENED_SHOWN || working(s)) : kin;
    if (from) from.hidden = kin.length - drawn.length;
    for (const session of drawn) {
      const shown = row(session, null, depth);
      rows.push(shown);
      if (shown.open) openOut(sessionKeyOf(session), depth + 1, shown);
    }
  };
  openOut(level, 0, null);
  return { rows, trail, shown: childrenOf(level).length, total };
}
