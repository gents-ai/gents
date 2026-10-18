import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { sessionKeyOf } from "../../hooks/fleetStore";
import { isLive } from "./live";

export type SessionRow =
  | {
      kind: "session";
      session: SessionSummary;
      child: boolean;
      /** shown only for what it started: it does not match itself */
      context: boolean;
      delay?: number;
    }
  | {
      kind: "toggle";
      /** the key of the session whose started sessions it opens */
      of: string;
      kin: number;
      open: boolean;
      running: number;
    };

/* an entrance, not a wait: a long group arrives as fast as a short one */
const ENTRANCE_STEPS = 6;

/**
 * The rows for `sessions` (the sessions in scope, in list order).
 *
 * - A session started by another one in the list sits under the top session
 *   of its line, however deep, through starters the list leaves out
 *   (`parentOf` gives the nearest one listed); one with none listed is a
 *   top row.
 * - A top row with started sessions always has a toggle, closed unless the
 *   person opened it (`isOpen`); a list someone is reading does not
 *   rearrange itself as workers start and settle.
 * - While the list is narrowed (`matches`), a top row shows when it or
 *   anything it started matches; one shown only for what it started is
 *   context. Its matching started sessions show under it even while it is
 *   closed, without opening it; under an open row the rest read as context.
 *
 * `shown` counts the sessions that match (every session when not narrowed).
 */
export function sessionRows({
  sessions,
  parentOf,
  matches,
  isOpen,
}: {
  sessions: readonly SessionSummary[];
  parentOf: (session: SessionSummary) => SessionSummary | null;
  /** null when nothing narrows the list */
  matches: ((session: SessionSummary) => boolean) | null;
  isOpen: (key: string) => boolean;
}): { rows: SessionRow[]; shown: number } {
  const listed = new Map(sessions.map((s) => [sessionKeyOf(s), s]));
  const topOf = (session: SessionSummary) => {
    let top = session;
    const seen = new Set([sessionKeyOf(session)]);
    for (;;) {
      const parent = parentOf(top);
      const key = parent && sessionKeyOf(parent);
      const next = key ? listed.get(key) : undefined;
      if (!key || !next) return top;
      /* a loop in who started whom has no top: each stands on its own */
      if (seen.has(key)) return session;
      seen.add(key);
      top = next;
    }
  };
  const tops: SessionSummary[] = [];
  const started = new Map<string, SessionSummary[]>();
  for (const session of sessions) {
    const top = topOf(session);
    if (top === session) {
      tops.push(session);
      continue;
    }
    const key = sessionKeyOf(top);
    const kin = started.get(key);
    if (kin) kin.push(session);
    else started.set(key, [session]);
  }
  const match = (session: SessionSummary) => !matches || matches(session);
  const rows: SessionRow[] = [];
  let shown = 0;
  for (const top of tops) {
    const key = sessionKeyOf(top);
    const kin = started.get(key) ?? [];
    const topMatches = match(top);
    const kinMatching = kin.filter(match);
    shown += (topMatches ? 1 : 0) + kinMatching.length;
    if (!topMatches && kinMatching.length === 0) continue;
    rows.push({ kind: "session", session: top, child: false, context: !topMatches });
    if (kin.length === 0) continue;
    const open = isOpen(key);
    const drawn = open ? kin : matches ? kinMatching : [];
    drawn.forEach((session, n) =>
      rows.push({
        kind: "session",
        session,
        child: true,
        context: !kinMatching.includes(session),
        delay: Math.min(n, ENTRANCE_STEPS),
      }),
    );
    rows.push({
      kind: "toggle",
      of: key,
      kin: kin.length,
      open,
      running: kin.filter((s) => isLive(s.turnState)).length,
    });
  }
  return { rows, shown };
}
