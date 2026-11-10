import type {
  GoalControlView,
  SessionGoalNextAction,
  SessionGoalState,
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

/** What the open chat shows for its Goal (Lean `SessionObservation.view`).
 * `goal` and `none` are the verified observation's facts; `unavailable`
 * names its cause and offers no Goal action; `pending` is a chat whose
 * snapshot has not been read yet, which shows nothing. */
export type GoalShown =
  | { kind: "goal"; goal: GoalControlView }
  | { kind: "none" }
  | { kind: "unavailable"; text: string; hold: string }
  | { kind: "pending" };

/** `undefined` is a chat whose snapshot has not arrived; `null` is a snapshot
 * whose read of the Goal state did not complete, which is no state. */
export function goalShown(state: SessionGoalState | null | undefined): GoalShown {
  if (state === undefined) return { kind: "pending" };
  if (state === null)
    return {
      kind: "unavailable",
      text: "This chat's Goal state could not be read from its runtime.",
      hold: "Available once this chat's goal can be read",
    };
  switch (state.kind) {
    case "observed":
      return state.observation.goal
        ? { kind: "goal", goal: state.observation.goal }
        : { kind: "none" };
    case "no_readable_session":
      return {
        kind: "unavailable",
        text: GOAL_NEXT_STEP.send,
        hold: "Available after you send a message",
      };
    case "unknown":
      return {
        kind: "unavailable",
        text: state.reason,
        hold: "Available once the agent publishes this chat's goal",
      };
  }
}
