export type TerminalTurnState =
  "completed" | "failed" | "superseded" | "interrupted";

export function isTerminalTurnState(
  turnState?: string | null,
): turnState is TerminalTurnState {
  return (
    turnState === "completed" ||
    turnState === "failed" ||
    turnState === "superseded" ||
    turnState === "interrupted"
  );
}
