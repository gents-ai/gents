import type { DesktopSessionSnapshot } from "./generated/DesktopSessionSnapshot.js";
import type { GoalControlView } from "./generated/GoalControlView.js";
import type { GoalRevision } from "./generated/GoalRevision.js";
import type { SessionGoalNextAction } from "./generated/SessionGoalNextAction.js";
import type { SessionGoalState } from "./generated/SessionGoalState.js";

/** One snapshot refresh's read of a session's Goal state (Lean
 * `SessionObservation.ReadOutcome`). The bridge sends a completed read as the
 * snapshot's `goal` and one that did not complete as `null`; a refused read
 * fails the whole snapshot. */
export type SessionGoalRead =
  | {
      kind: "completed";
      sessionId: string;
      nodeDid: string | null;
      state: SessionGoalState;
    }
  | { kind: "incomplete"; sessionId: string; nodeDid: string | null }
  | { kind: "refused" };

/** What the client holds for the one session it last read (Lean
 * `SessionObservation.Held`). */
export type HeldSessionGoal = {
  sessionId: string;
  nodeDid: string | null;
  /** The latest completed read of this session. */
  last: SessionGoalState | null;
  /** Whether the latest read of this session completed. */
  current: boolean;
};

/** A session's Goal view from one completed read (Lean
 * `SessionObservation.view`). */
export type SessionGoalStateView =
  | { kind: "goal"; goal: GoalControlView }
  | { kind: "none" }
  | {
      kind: "unavailable";
      state: Exclude<SessionGoalState, { kind: "observed" }>;
    };

/** What the person is told about a session's Goal (Lean
 * `SessionObservation.Shown`). Only `current` is a fact about the Goal now. */
export type SessionGoalShown =
  | { kind: "pending" }
  | { kind: "unread" }
  | { kind: "current"; view: SessionGoalStateView }
  | { kind: "stale"; view: SessionGoalStateView };

/** The Goal action offered and the revision it fences on (Lean
 * `SessionObservation.offered`). */
export type SessionGoalControl = {
  next: SessionGoalNextAction;
  revision: GoalRevision | null;
};

export function sessionGoalReadOf(
  snapshot: DesktopSessionSnapshot,
): SessionGoalRead {
  const { sessionId, nodeDid } = snapshot;
  return snapshot.goal == null
    ? { kind: "incomplete", sessionId, nodeDid }
    : { kind: "completed", sessionId, nodeDid, state: snapshot.goal };
}

const sameSession = (
  held: { sessionId: string; nodeDid: string | null },
  sessionId: string,
  nodeDid: string | null,
) =>
  held.sessionId === sessionId && (held.nodeDid ?? null) === (nodeDid ?? null);

/** Lean `SessionObservation.step`. */
export function foldSessionGoalRead(
  held: HeldSessionGoal | null,
  read: SessionGoalRead,
): HeldSessionGoal | null {
  switch (read.kind) {
    case "refused":
      return null;
    case "completed":
      return {
        sessionId: read.sessionId,
        nodeDid: read.nodeDid,
        last: read.state,
        current: true,
      };
    case "incomplete":
      return held && sameSession(held, read.sessionId, read.nodeDid)
        ? { ...held, current: false }
        : {
            sessionId: read.sessionId,
            nodeDid: read.nodeDid,
            last: null,
            current: false,
          };
  }
}

/** Lean `SessionObservation.view`. */
export function sessionGoalStateView(
  state: SessionGoalState,
): SessionGoalStateView {
  if (state.kind !== "observed") return { kind: "unavailable", state };
  return state.observation.goal
    ? { kind: "goal", goal: state.observation.goal }
    : { kind: "none" };
}

/** Lean `SessionObservation.shown`. */
export function sessionGoalShown(
  held: HeldSessionGoal | null,
  sessionId: string,
  nodeDid: string | null,
): SessionGoalShown {
  if (!held || !sameSession(held, sessionId, nodeDid))
    return { kind: "pending" };
  if (!held.last) return { kind: "unread" };
  const view = sessionGoalStateView(held.last);
  return held.current ? { kind: "current", view } : { kind: "stale", view };
}

/** Lean `SessionObservation.offered`: only a current view offers its next
 * action, fenced on the revision of the Goal it shows. */
export function sessionGoalControl(
  held: HeldSessionGoal | null,
  sessionId: string,
  nodeDid: string | null,
): SessionGoalControl | null {
  if (!held?.last || !held.current || !sameSession(held, sessionId, nodeDid))
    return null;
  const state = held.last;
  return {
    next: state.next,
    revision:
      state.kind === "observed"
        ? (state.observation.goal?.revision ?? null)
        : null,
  };
}

/** The Goal one snapshot shows on its own: its read, folded from nothing. */
export function snapshotGoalShown(
  snapshot: DesktopSessionSnapshot,
): SessionGoalShown {
  return sessionGoalShown(
    foldSessionGoalRead(null, sessionGoalReadOf(snapshot)),
    snapshot.sessionId,
    snapshot.nodeDid,
  );
}
