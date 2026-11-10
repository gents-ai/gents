import type {
  GoalControlView,
  SessionGoalState,
} from "@source-inc/gents-desktop-client";

export const GOAL_REVISION = { document: "goal-doc", composite: "cid-1" };

/** A Goal as the verified observation carries it. */
export function goalControl(
  status: string,
  over: Partial<GoalControlView> = {},
): GoalControlView {
  return {
    goalId: "g",
    revision: GOAL_REVISION,
    objective: "Ship the release",
    status,
    tokenBudget: null,
    tokensUsed: "0",
    activeTimeSeconds: "0",
    continuationSequence: "0",
    wrapupRequested: false,
    wrapupCompleted: status === "complete",
    lastBlockedReason: null,
    lastFailure: null,
    completionEvidence: null,
    ...over,
  };
}

/** The observed state for `goal`, or verified absence when it is null. */
export function observedGoal(
  goal: GoalControlView | null,
  { nodeDid = "did:test:node", sessionId = "session" } = {},
): SessionGoalState {
  return {
    kind: "observed",
    observation: {
      nodeDid,
      sessionId,
      sessionDocId: "session-doc",
      observedAt: "2026-11-08T00:00:00Z",
      goal,
    },
    next: goal ? "control_goal" : "start_goal",
  };
}
