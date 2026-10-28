import { actionKind, type ActionEntry } from "./actionPresentation.js";

export type ActionCategory =
  "read" | "explore" | "search" | "edit" | "command" | "action";

/** Categories describe reconstructed operations, not their success or effect. */
export function actionCategory(entry: ActionEntry): ActionCategory {
  if (actionKind(entry) === "command") return "command";
  const { tool } = entry;
  if (tool.reconstruction.state !== "ready") return "action";
  const view = tool.presentation;
  if (view.kind === "fileEdit") return "edit";
  if (view.kind !== "fileRead") return "action";
  switch (view.operation) {
    case "grep":
    case "glob":
      return "search";
    case "read_file":
      return "read";
    default:
      return "explore";
  }
}

const LABELS: Record<ActionCategory, [string, string]> = {
  read: ["file read", "file reads"],
  explore: ["exploration", "explorations"],
  search: ["search", "searches"],
  edit: ["file edit", "file edits"],
  command: ["command", "commands"],
  action: ["action", "actions"],
};

export function completedActionLabel(entries: ActionEntry[]): string {
  const counts = new Map<ActionCategory, number>();
  for (const entry of entries) {
    const category = actionCategory(entry);
    counts.set(category, (counts.get(category) ?? 0) + 1);
  }
  return [...counts]
    .map(
      ([category, count]) =>
        `${count} ${LABELS[category][count === 1 ? 0 : 1]}`,
    )
    .join(", ");
}

/** One icon represents the whole row; never hide a file edit behind a read. */
export function representativeActionCategory(
  entries: ActionEntry[],
): ActionCategory {
  const categories = new Set(entries.map(actionCategory));
  return (
    (["edit", "command", "search", "read", "explore", "action"] as const).find(
      (category) => categories.has(category),
    ) ?? "action"
  );
}
