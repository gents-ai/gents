/* What the sessions list is narrowed to, besides its nodes. */
import { storedStrings } from "./storage";

export type SessionState = "live" | "failed";
export type SessionSource = "person" | "task" | "session";

export type SessionFilter = {
  states: SessionState[];
  sources: SessionSource[];
  behaviors: string[];
};

export const emptyFilter: SessionFilter = {
  states: [],
  sources: [],
  behaviors: [],
};

export const hasFilter = (f: SessionFilter) =>
  f.states.length > 0 || f.sources.length > 0 || f.behaviors.length > 0;

const STATES: readonly string[] = ["live", "failed"];
const SOURCES: readonly string[] = ["person", "task", "session"];

/** A stored filter, read back: a state or source this build does not offer
    is dropped, not misread. */
export function restoreSessionFilter(stored: unknown): SessionFilter {
  const saved = typeof stored === "object" && stored !== null ? stored : {};
  const field = (key: string) => storedStrings((saved as Record<string, unknown>)[key]);
  return {
    states: field("states").filter((s): s is SessionState => STATES.includes(s)),
    sources: field("sources").filter((s): s is SessionSource => SOURCES.includes(s)),
    behaviors: field("behaviors"),
  };
}
