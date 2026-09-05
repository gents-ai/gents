export type TerminalTurnState =
  | "completed"
  | "failed"
  | "superseded"
  | "interrupted";

/**
 * True for the four terminal turn-state labels the bridge derives from
 * `ClientTurnState::is_terminal` (Rust: `gents_protocol::client_protocol`).
 * Single owner for the desktop-chat and desktop-fleet copies (#1339).
 */
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
