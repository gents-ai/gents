import type { SessionState } from "./live";

export { stateOf, type SessionState } from "./live";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { stateOf } from "./live";
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

export const matchesState = (c: SessionSummary, state: SessionState) =>
  stateOf(c.turnState) === state;

export const matchesSource = (c: SessionSummary, source: SessionSource) =>
  source === "task"
    ? Boolean(c.taskId || c.triggerId)
    : source === "session"
      ? Boolean(c.startedBy)
      : !(c.taskId || c.triggerId || c.startedBy);

export const passes = (c: SessionSummary, f: SessionFilter) =>
  (f.states.length === 0 || f.states.some((s) => matchesState(c, s))) &&
  (f.sources.length === 0 || f.sources.some((s) => matchesSource(c, s))) &&
  (f.agents.length === 0 || f.agents.includes(c.agentId ?? ""));
