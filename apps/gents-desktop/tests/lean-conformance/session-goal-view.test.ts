import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";
import {
  mergeSessionTipSnapshot,
  type DesktopSessionSnapshot,
  type SessionGoalNextAction,
  type SessionGoalState,
} from "@source-inc/gents-desktop-client";

import { GOAL_NEXT_STEP, goalShown } from "../../src/hooks/goalState";
import { goalCommand } from "../../src/ui/screens/session/goalMode";
import { goalControl, observedGoal } from "../goal-state-fixture";

type Reference = { goal: number; root: number | null };
type Case = {
  session: string;
  projection: unknown;
  expected_state:
    | { kind: "no_readable_session" }
    | { kind: "unknown"; cause: "projection_missing" | "projection_unverified" }
    | { kind: "observed"; goal: Reference | null };
  expected_view:
    | { kind: "goal"; goal: Reference }
    | { kind: "no_goal" }
    | { kind: "unavailable"; cause: string };
  expected_next: SessionGoalNextAction;
};
const run = promisify(execFile);

async function leanCases(): Promise<Case[]> {
  const cwd = join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../crates/gents/proofs",
  );
  await run("lake", ["build", "Proofs.Conformance.GoalSessionObservation"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "GoalSessionObservationConformance.lean"],
    { cwd, timeout: 240_000 },
  );
  return JSON.parse(stdout) as Case[];
}

/* The modeled state as the bridge carries it: the state and its `next` from
   the runtime owner, and an unknown state's refusal text. */
function wire(row: Case): SessionGoalState {
  const state = row.expected_state;
  switch (state.kind) {
    case "no_readable_session":
      return { kind: "no_readable_session", next: row.expected_next };
    case "unknown":
      return {
        kind: "unknown",
        cause: state.cause,
        next: row.expected_next,
        reason: `refused: ${state.cause}`,
      };
    case "observed":
      return {
        ...observedGoal(
          state.goal
            ? goalControl("active", { goalId: `goal:${state.goal.goal}` })
            : null,
        ),
        next: row.expected_next,
      };
  }
}

const snapshot = (goal: SessionGoalState | null, sessionId = "session") =>
  ({
    sessionId,
    nodeDid: "did:test:node",
    timelineItems: [],
    goal,
  }) as unknown as DesktopSessionSnapshot;

describe("session Goal view", () => {
  it("shows each modeled Goal state as its view and offers only its next action", async () => {
    const cases = await leanCases();
    expect(cases).toHaveLength(20);
    for (const kind of ["goal", "no_goal", "unavailable"])
      expect(
        cases.some((row) => row.expected_view.kind === kind),
        kind,
      ).toBe(true);
    for (const row of cases) {
      const label = JSON.stringify(row);
      const state = wire(row);
      const shown = goalShown(state);
      const command = goalCommand(shown, false);
      const view = row.expected_view;
      switch (view.kind) {
        case "goal":
          expect(shown, label).toMatchObject({
            kind: "goal",
            goal: { goalId: `goal:${view.goal.goal}` },
          });
          expect(row.expected_next, label).toBe("control_goal");
          expect(command.disabled, label).toBeFalsy();
          break;
        case "no_goal":
          expect(shown, label).toEqual({ kind: "none" });
          expect(row.expected_next, label).toBe("start_goal");
          expect(command, label).toEqual({
            id: "goal",
            description: "Set a goal the agent works toward until it's met",
          });
          break;
        case "unavailable":
          expect(shown.kind, label).toBe("unavailable");
          expect(command.disabled, label).toBe(true);
          expect(shown, label).toMatchObject({
            text:
              view.cause === "session_not_readable"
                ? GOAL_NEXT_STEP.send
                : `refused: ${view.cause}`,
          });
          break;
      }
    }
  }, 300_000);

  it("keeps the last read Goal state until another read classifies one", async () => {
    const cases = await leanCases();
    for (const before of cases)
      for (const after of cases) {
        const label = JSON.stringify({ before, after });
        const held = snapshot(wire(before));
        expect(
          mergeSessionTipSnapshot(held, snapshot(null)).goal,
          `a read that did not complete is no state: ${label}`,
        ).toEqual(held.goal);
        expect(
          mergeSessionTipSnapshot(held, snapshot(wire(after))).goal,
          `a completed read replaces it: ${label}`,
        ).toEqual(wire(after));
        expect(
          mergeSessionTipSnapshot(held, snapshot(null, "other")).goal,
          `another chat's state is not carried: ${label}`,
        ).toBeNull();
      }
  }, 300_000);
});
