import type { ActionCategory } from "./actionSummary.js";

const PATHS: Record<ActionCategory, string> = {
  read: "M4 4h6a3 3 0 0 1 2 1 3 3 0 0 1 2-1h6v15h-6a3 3 0 0 0-2 1 3 3 0 0 0-2-1H4V4Zm8 1v15",
  explore: "M3 7V5h6l2 2h10v12H3V7Z",
  search: "M16 10a6 6 0 1 1-12 0 6 6 0 0 1 12 0Zm-2 4 6 6",
  edit: "m14 5 5 5M4 20l5-1L20 8l-5-5L4 14v6Z",
  command: "m4 5 7 7-7 7m9 0h7",
  action: "m12 3 9 9-9 9-9-9 9-9Z",
};

export function ActionCategoryIcon({
  category,
  running,
}: {
  category: ActionCategory;
  running: boolean;
}) {
  return (
    <svg
      className="tool-activity-icon"
      data-action-category={running ? "running" : category}
      viewBox="0 0 24 24"
      aria-hidden="true"
    >
      {running ? (
        <circle cx="12" cy="12" r="8" />
      ) : (
        <path d={PATHS[category]} />
      )}
    </svg>
  );
}
