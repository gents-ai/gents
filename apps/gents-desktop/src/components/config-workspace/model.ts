export type ConfigTab =
  | "agent"
  | "behavior"
  | "skills"
  | "backends"
  | "providerAccounts"
  | "profiles"
  | "toolSelections"
  | "metaTools"
  | "tasks"
  | "timerTriggers"
  | "eventTriggers";

export const TABS: Array<{ id: ConfigTab; label: string }> = [
  { id: "agent", label: "Agent" },
  { id: "behavior", label: "Behavior" },
  { id: "skills", label: "Skills" },
  { id: "backends", label: "Backends" },
  { id: "providerAccounts", label: "Provider Accounts" },
  { id: "profiles", label: "Profiles" },
  { id: "toolSelections", label: "Tool Selections" },
  { id: "metaTools", label: "Meta Tools" },
  { id: "tasks", label: "Tasks" },
  { id: "timerTriggers", label: "Timer Triggers" },
  { id: "eventTriggers", label: "Event Triggers" },
];

export const NEW_DOCUMENT_ID = "__new__";

export function ensureSelection(
  current: string | null,
  fallback: string | null,
  exists: (id: string) => boolean,
  setSelection: (id: string | null) => void,
) {
  if (current === NEW_DOCUMENT_ID) {
    return;
  }
  if (current && exists(current)) {
    return;
  }
  if (current !== fallback) {
    setSelection(fallback);
  }
}
