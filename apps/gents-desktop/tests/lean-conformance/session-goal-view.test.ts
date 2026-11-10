import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";
import {
  foldSessionGoalRead,
  sessionGoalControl,
  sessionGoalShown,
  type GoalRevision,
  type HeldSessionGoal,
  type SessionGoalNextAction,
  type SessionGoalRead,
  type SessionGoalState,
} from "@source-inc/gents-desktop-client";

import { GOAL_NEXT_STEP, goalShown } from "../../src/hooks/goalState";
import { goalCommand } from "../../src/ui/screens/session/goalMode";
import { goalControl, observedGoal } from "../goal-state-fixture";

type Reference = { goal: number; root: number | null };
type ModeledState =
  | { kind: "no_readable_session" }
  | { kind: "unknown"; cause: "projection_missing" | "projection_unverified" }
  | { kind: "observed"; goal: Reference | null };
type ModeledView =
  | { kind: "goal"; goal: Reference }
  | { kind: "no_goal" }
  | { kind: "unavailable"; cause: string };
type Case = {
  expected_state: ModeledState;
  expected_view: ModeledView;
  expected_next: SessionGoalNextAction;
};
type ModeledRead = {
  session: number;
  outcome:
    | {
        kind: "completed";
        state: ModeledState;
        next: SessionGoalNextAction;
        revision: GoalRevision | null;
      }
    | { kind: "incomplete" }
    | { kind: "refused" };
};
type FoldCase = {
  reads: ModeledRead[];
  session: number;
  expected_shown:
    { kind: "pending" | "unread" } | { kind: "current" | "stale"; view: ModeledView };
  expected_offered: {
    next: SessionGoalNextAction;
    revision: GoalRevision | null;
  } | null;
};
const run = promisify(execFile);
const NODE = "did:test:node";
const sessionIdOf = (session: number) => `session-${session}`;

let generated: Promise<{ cases: Case[]; fold_cases: FoldCase[] }> | null = null;
function leanCases() {
  generated ??= (async () => {
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
    return JSON.parse(stdout);
  })();
  return generated;
}

/* The modeled state as the bridge carries it: the owner's state and `next`,
   an unknown state's refusal text, and the shown Goal's revision. */
function wire(
  state: ModeledState,
  next: SessionGoalNextAction,
  sessionId = "session",
  revision: GoalRevision | null = null,
): SessionGoalState {
  switch (state.kind) {
    case "no_readable_session":
      return { kind: "no_readable_session", next };
    case "unknown":
      return {
        kind: "unknown",
        cause: state.cause,
        next,
        reason: `refused: ${state.cause}`,
      };
    case "observed":
      return {
        ...observedGoal(
          state.goal
            ? goalControl("active", {
                goalId: `goal:${state.goal.goal}`,
                ...(revision ? { revision } : {}),
              })
            : null,
          { nodeDid: NODE, sessionId },
        ),
        next,
      };
  }
}

function readOf({ session, outcome }: ModeledRead): SessionGoalRead {
  const sessionId = sessionIdOf(session);
  switch (outcome.kind) {
    case "refused":
      return { kind: "refused" };
    case "incomplete":
      return { kind: "incomplete", sessionId, nodeDid: NODE };
    case "completed":
      return {
        kind: "completed",
        sessionId,
        nodeDid: NODE,
        state: wire(outcome.state, outcome.next, sessionId, outcome.revision),
      };
  }
}

const UNAVAILABLE_TEXT = (cause: string) =>
  cause === "session_not_readable" ? GOAL_NEXT_STEP.send : `refused: ${cause}`;

describe("session Goal view", () => {
  it("shows each modeled Goal state as its view and offers only its next action", async () => {
    const { cases } = await leanCases();
    expect(cases).toHaveLength(20);
    for (const kind of ["goal", "no_goal", "unavailable"])
      expect(
        cases.some((row) => row.expected_view.kind === kind),
        kind,
      ).toBe(true);
    for (const row of cases) {
      const label = JSON.stringify(row);
      const held = foldSessionGoalRead(null, {
        kind: "completed",
        sessionId: "session",
        nodeDid: NODE,
        state: wire(row.expected_state, row.expected_next),
      });
      const shown = goalShown(sessionGoalShown(held, "session", NODE));
      const command = goalCommand(shown, false);
      const view = row.expected_view;
      switch (view.kind) {
        case "goal":
          expect(shown, label).toMatchObject({
            kind: "goal",
            current: true,
            goal: { goalId: `goal:${view.goal.goal}` },
          });
          expect(command.disabled, label).toBeFalsy();
          break;
        case "no_goal":
          expect(shown, label).toEqual({ kind: "none" });
          expect(command, label).toEqual({
            id: "goal",
            description: "Set a Goal the Agent works toward until it's met",
          });
          break;
        case "unavailable":
          expect(shown, label).toMatchObject({
            kind: "unavailable",
            label: "Goal unknown",
            text: UNAVAILABLE_TEXT(view.cause),
          });
          expect(command.disabled, label).toBe(true);
          break;
      }
      expect(sessionGoalControl(held, "session", NODE)?.next, label).toBe(
        row.expected_next,
      );
    }
  }, 300_000);

  it("shows and offers what the modeled fold of reads does", async () => {
    const { fold_cases } = await leanCases();
    expect(fold_cases.length).toBeGreaterThan(500);
    for (const kind of ["pending", "unread", "current", "stale"])
      expect(
        fold_cases.some((row) => row.expected_shown.kind === kind),
        kind,
      ).toBe(true);
    for (const row of fold_cases) {
      const label = JSON.stringify(row);
      const held = row.reads
        .map(readOf)
        .reduce<HeldSessionGoal | null>(foldSessionGoalRead, null);
      const sessionId = sessionIdOf(row.session);
      const told = sessionGoalShown(held, sessionId, NODE);
      const expected = row.expected_shown;
      expect(told.kind, label).toBe(expected.kind);
      if ((told.kind === "current" || told.kind === "stale") && "view" in expected) {
        const view = expected.view;
        expect(told.view.kind, label).toBe(
          view.kind === "no_goal" ? "none" : view.kind,
        );
        if (view.kind === "goal" && told.view.kind === "goal")
          expect(told.view.goal.goalId, label).toBe(`goal:${view.goal.goal}`);
        if (view.kind === "unavailable" && told.view.kind === "unavailable")
          expect(
            told.view.state.kind === "unknown"
              ? told.view.state.cause
              : "session_not_readable",
            label,
          ).toBe(view.cause);
      }
      expect(sessionGoalControl(held, sessionId, NODE), label).toEqual(
        row.expected_offered,
      );

      /* the app offers `/goal` and Goal controls only where Lean offers an
         action, and never tells a stale or unread state as current */
      const shown = goalShown(told);
      const command = goalCommand(shown, false);
      if (!row.expected_offered) {
        expect(command.disabled, label).toBe(true);
        expect(shown.kind === "goal" && shown.current, label).toBe(false);
        expect(shown.kind === "none", label).toBe(false);
      }
      if (expected.kind === "stale" && expected.view.kind === "goal")
        expect(shown, label).toMatchObject({ kind: "goal", current: false });
    }
  }, 300_000);
});
