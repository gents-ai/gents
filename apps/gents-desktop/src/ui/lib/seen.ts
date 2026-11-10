import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { useSeenMark, useSeenSince, type SeenMark } from "@/app/sessionViews";
import { sessionKeyOf } from "../../hooks/fleetStore";
import { stateOf } from "./live";

type Seeable = Pick<
  SessionSummary,
  "latestRequestId" | "turnState" | "updatedAt" | "startedBy"
>;
type IdentifiedSession = Pick<SessionSummary, "sessionId" | "nodeDid" | "requesterDid">;

const ended = (s: Seeable) => {
  const state = stateOf(s.turnState);
  return state !== "working" && state !== "waiting";
};

/** What a session has done, as a mark to keep when it is on screen. */
export const seenMarkOf = (s: Seeable): SeenMark => ({
  request: s.latestRequestId,
  done: ended(s),
});

/** Whether `s` finished work since it was last on screen here.

    It follows the latest request, not the activity time: a rename, a
    generated title or closing a session moves that time without new work.
    One never seen here counts only work after `since`. */
export function finishedSince(
  s: Seeable,
  seen: SeenMark | null,
  since: string | null,
): boolean {
  if (!s.latestRequestId || !ended(s)) return false;
  if (seen) return seen.request !== s.latestRequestId || !seen.done;
  const at = Date.parse(s.updatedAt ?? "");
  return since !== null && at > Date.parse(since);
}

/** `finishedSince` for a session in a list of sessions. A session another
    session started has no mark of its own there; its starter's covers it. */
export const isNew = (s: Seeable, seen: SeenMark | null, since: string | null) =>
  !s.startedBy && finishedSince(s, seen, since);

/** `finishedSince` for one session, following its mark and the device's start. */
export function useFinishedSince(s: Seeable & IdentifiedSession): boolean {
  const seen = useSeenMark(sessionKeyOf(s));
  const since = useSeenSince();
  return finishedSince(s, seen, since);
}

/** `isNew` for one session, following its mark and the device's start. */
export function useIsNew(s: Seeable & IdentifiedSession): boolean {
  const fresh = useFinishedSince(s);
  return !s.startedBy && fresh;
}
