import type {
  GoalControlView,
  SessionGoalNextAction,
  SessionGoalShown,
  SessionGoalStateView,
} from "@source-inc/gents-desktop-client";

/** What to do instead when a state's next action is not the one requested.
 * An unknown state carries its own reason from the runtime owner. */
export const GOAL_NEXT_STEP: Record<
  Exclude<SessionGoalNextAction, "await_publication">,
  string
> = {
  send: "This chat is not open on its runtime yet. Send a message to open it, then change its Goal.",
  start_goal: "This chat has no Goal to change. Start one first.",
  control_goal: "This chat already has a Goal. Change it instead.",
};

/** A Goal change refused because what was shown is not the Goal now. */
export const GOAL_CHANGED =
  "This Goal changed since it was shown. Its current state is shown now.";
export const GOAL_NOT_CURRENT =
  "This chat's Goal isn't current. Try again once it refreshes.";

const NOT_REFRESHED = "This chat's Goal could not be refreshed from its runtime.";
const REFRESH_HOLD = "Available once this chat's Goal is refreshed";

/** How the open chat presents what it is told about its Goal (Lean
 * `SessionObservation.shown`). Only a current Goal or current absence offers
 * a Goal action; a stale Goal is shown, marked, with none. */
export type GoalShown =
  | { kind: "goal"; goal: GoalControlView; current: boolean }
  | { kind: "none" }
  | { kind: "unavailable"; label: string; text: string; hold: string }
  | { kind: "pending" };

function currentView(view: SessionGoalStateView): GoalShown {
  switch (view.kind) {
    case "goal":
      return { kind: "goal", goal: view.goal, current: true };
    case "none":
      return { kind: "none" };
    case "unavailable":
      return view.state.kind === "no_readable_session"
        ? {
            kind: "unavailable",
            label: "Goal unknown",
            text: GOAL_NEXT_STEP.send,
            hold: "Available after you send a message",
          }
        : {
            kind: "unavailable",
            label: "Goal unknown",
            text: view.state.reason,
            hold: "Available once the Agent publishes this chat's Goal",
          };
  }
}

function staleView(view: SessionGoalStateView): GoalShown {
  switch (view.kind) {
    case "goal":
      return { kind: "goal", goal: view.goal, current: false };
    case "none":
      return {
        kind: "unavailable",
        label: "Goal not current",
        text: `${NOT_REFRESHED} When last read, it had no Goal.`,
        hold: REFRESH_HOLD,
      };
    case "unavailable":
      return {
        kind: "unavailable",
        label: "Goal not current",
        text: `${NOT_REFRESHED} When last read, its Goal state was not known.`,
        hold: REFRESH_HOLD,
      };
  }
}

export function goalShown(shown: SessionGoalShown): GoalShown {
  switch (shown.kind) {
    case "pending":
      return { kind: "pending" };
    case "unread":
      return {
        kind: "unavailable",
        label: "Goal unknown",
        text: "This chat's Goal could not be read from its runtime.",
        hold: "Available once this chat's Goal can be read",
      };
    case "current":
      return currentView(shown.view);
    case "stale":
      return staleView(shown.view);
  }
}

/** The hint a stale Goal's `/goal` shows. */
export const GOAL_STALE_HOLD = REFRESH_HOLD;
