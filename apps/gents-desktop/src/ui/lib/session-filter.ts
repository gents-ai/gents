/* What the sessions list is narrowed to, besides its nodes. */
import type { SessionState } from "./live";

export { stateOf, type SessionState } from "./live";
import { storedStrings } from "./storage";

export type SessionSource = "person" | "task" | "session";

export type SessionFilter = {
  states: SessionState[];
  sources: SessionSource[];
  agents: string[];
};

export const emptyFilter: SessionFilter = {
  states: [],
  sources: [],
  agents: [],
};

export const hasFilter = (f: SessionFilter) =>
  f.states.length > 0 || f.sources.length > 0 || f.agents.length > 0;

const STATES: readonly string[] = [
  "working",
  "waiting",
  "failed",
  "stopped",
  "finished",
];
const SOURCES: readonly string[] = ["person", "task", "session"];

/** A stored filter, read back: a state or source this build does not offer
    is dropped, not misread. Storage can hold "live", every unsettled
    session, which is working or waiting. */
export function restoreSessionFilter(stored: unknown): SessionFilter {
  const saved = typeof stored === "object" && stored !== null ? stored : {};
  const field = (key: string) => storedStrings((saved as Record<string, unknown>)[key]);
  const states = field("states").flatMap((s) =>
    s === "live" ? ["working", "waiting"] : [s],
  );
  return {
    states: [...new Set(states)].filter((s): s is SessionState => STATES.includes(s)),
    sources: field("sources").filter((s): s is SessionSource => SOURCES.includes(s)),
    agents: field("agents"),
  };
}
