/* the session open in this window and the one open before it, for the key
   that goes back to it; it isn't kept across a reload */
let current: string | null = null;
let previous: string | null = null;

/** Notes a session opened, so the one before it can be gone back to. */
export function noteOpenSession(sessionId: string) {
  if (sessionId === current) return;
  if (current) previous = current;
  current = sessionId;
}

/** The session to go back to from `here`: the one open before it, or from
    a page that isn't a session, the one open last. */
export function sessionToReturnTo(here: string | null): string | null {
  return here !== null && here === current ? previous : current;
}

export function resetSessionTrail() {
  current = null;
  previous = null;
}
