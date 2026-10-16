import { storedStrings } from "./storage";

export type SessionState = "live" | "failed";
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

const STATES: readonly string[] = ["live", "failed"];
const SOURCES: readonly string[] = ["person", "task", "session"];

export function restoreSessionFilter(stored: unknown): SessionFilter {
  const saved = typeof stored === "object" && stored !== null ? stored : {};
  const field = (key: string) => storedStrings((saved as Record<string, unknown>)[key]);
  return {
    states: field("states").filter((s): s is SessionState => STATES.includes(s)),
    sources: field("sources").filter((s): s is SessionSource => SOURCES.includes(s)),
    agents: field("agents"),
  };
}
