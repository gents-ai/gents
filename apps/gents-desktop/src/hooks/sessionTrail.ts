/* the sessions opened in this window, in order, and where in them it is, for
   the keys that go back and forward through them; it isn't kept across a
   reload */
const KEPT = 50;
let trail: string[] = [];
let at = -1;
/* a step back or forward on its way: when its session opens, the trail
   moves there rather than adding it, so the sessions ahead stay */
let stepping: { sessionId: string; index: number } | null = null;

/** Notes a session opened. Opened by going back or forward, the trail moves
    to it; opened any other way, it follows the one open now and the
    sessions ahead are dropped, as a page's history does. */
export function noteOpenSession(sessionId: string) {
  if (stepping?.sessionId === sessionId) {
    at = stepping.index;
    stepping = null;
    return;
  }
  stepping = null;
  if (trail[at] === sessionId) return;
  trail = [...trail.slice(0, at + 1), sessionId].slice(-KEPT);
  at = trail.length - 1;
}

/** The session going back leads to from `here`: the one opened before it,
    or from a page that isn't a session, the one open last. */
export function sessionBack(here: string | null): string | null {
  if (here !== null && here === trail[at]) return trail[at - 1] ?? null;
  return trail[at] ?? null;
}

/** The session going forward leads to from `here`, after going back. */
export function sessionForward(here: string | null): string | null {
  return here !== null && here === trail[at] ? (trail[at + 1] ?? null) : null;
}

/** Marks a step back or forward to `sessionId`, before opening it. */
export function stepTo(sessionId: string, by: -1 | 0 | 1) {
  stepping = { sessionId, index: at + by };
}

export function resetSessionTrail() {
  trail = [];
  at = -1;
  stepping = null;
}
